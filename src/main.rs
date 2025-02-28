mod args;
mod dbg;
mod error;
mod format;
mod headers;
mod http;
mod io;
mod logger;
mod prop;
mod template;

use crate::args::Args;
use crate::dbg::dbg_info;
use crate::error::exit;
use crate::format::ContentFormatter;
use crate::http::HttpRequest;
use crate::io::write;
use crate::io::write_color;
use crate::io::writeln;
use crate::io::writeln_spec;
use crate::logger::setup_logging;
use crate::prop::Property;
use crate::template::substitution;
use clap::Parser;
use error::FireError;
use reqwest::blocking::Response;
use std::process::ExitCode;
use std::str::FromStr;
use std::time::Duration;
use std::time::Instant;
use template::SubstitutionError;
use termcolor::{Color, ColorSpec, StandardStream};

fn main() -> ExitCode {
    match exec() {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => exit(e),
    }
}

fn exec() -> Result<(), FireError> {
    let args: Args = Args::parse();
    setup_logging(args.verbosity_level);
    log::debug!("Config: {:?}", args);

    let mut stdout = StandardStream::stdout(args.use_colors());

    if args.print_dbg {
        write(&mut stdout, &dbg_info());
        return Ok(());
    }

    // 1. Read file content
    let file = match std::fs::read_to_string(args.file()) {
        Ok(file) => file,
        Err(e) => {
            return match e.kind() {
                std::io::ErrorKind::NotFound => {
                    Err(FireError::FileNotFound(args.file().to_path_buf()))
                }
                std::io::ErrorKind::PermissionDenied => {
                    Err(FireError::NoReadPermission(args.file().to_path_buf()))
                }
                _ => Err(FireError::GenericIO(e.to_string())),
            }
        }
    };
    // 2. Read enviroment variables from system environment and extra environments supplied via cli
    // 3. Apply template substitution
    let props: Vec<Property> = args.env().expect("Unable to load env vars");

    log::debug!("Received properties {:?}", props);

    let content: String = substitution(file, props)?;

    // 4. Parse Validate format of request
    let request: HttpRequest = HttpRequest::from_str(&content).unwrap();
    // 5. Add user-agent header if missing
    // 6. Add content-length header if missing
    // 7. Make (and optionally print) request
    let client = reqwest::blocking::Client::new();

    let syntax_hilighiting: bool = args.use_colors() != termcolor::ColorChoice::Never;
    let formatters: Vec<Box<dyn ContentFormatter>> = format::formatters(syntax_hilighiting);

    let req_headers = request.headers();

    let content_type: Option<&str> =
        req_headers.get("content-type").map(|h| h.to_str()).map(|v| v.unwrap());

    if args.print_request() {
        let title: String = format!("{} {}", request.verb(), request.url().unwrap());
        writeln(&mut stdout, &title);
        let border = "━".repeat(title.len());
        writeln(&mut stdout, &border);

        if args.headers {
            let mut spec = ColorSpec::new();
            spec.set_dimmed(true);
            for (k, v) in &req_headers {
                writeln_spec(&mut stdout, &format!("{}: {:?}", k.as_str(), v), &spec);
            }
            if request.body().is_some() {
                writeln(&mut stdout, "");
            }
        }

        if let Some(body) = request.body() {
            let content: String = formatters
                .iter()
                .filter(|fmt| fmt.accept(content_type))
                .fold(body.clone(), |content, fmt| fmt.format(content).unwrap());

            writeln(&mut stdout, &content);
        }
        writeln(&mut stdout, "");
    }

    let req = client
        .request(request.verb().into(), request.url().unwrap())
        .timeout(args.timeout())
        .headers(req_headers);

    let req = match request.body() {
        Some(body) => req.body(body.clone()).build().unwrap(),
        None => req.build().unwrap(),
    };

    let start: Instant = Instant::now();
    let resp: Result<Response, reqwest::Error> = client.execute(req);
    let end: Instant = Instant::now();
    let resp: Response = match resp {
        Ok(response) => response,
        Err(e) => {
            return if e.is_timeout() {
                Err(FireError::Timeout(e.url().unwrap().clone()))
            } else if e.is_connect() {
                Err(FireError::Connection(e.url().unwrap().clone()))
            } else {
                Err(FireError::Other(e.to_string()))
            }
        }
    };

    let duration: Duration = end.duration_since(start);
    // 8. Print response if successful, or error, if not

    let version = resp.version();
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = match resp.text() {
        Ok(body) => body,
        Err(e) => return Err(FireError::Other(e.to_string())),
    };

    log::debug!("Body of response:\n{body}");

    let status_color: Option<Color> = match status.as_u16() {
        200..=299 => Some(Color::Green),
        400..=499 => Some(Color::Yellow),
        500..=599 => Some(Color::Red),
        _ => None,
    };

    let (body_len, unit): (usize, String) = if body.len() >= 1024 {
        ((body.len() / 1024), String::from("kb"))
    } else {
        (body.len(), String::from("b"))
    };

    let version: String = format!("{version:?} ");
    write(&mut stdout, &version);

    let status: String = status.to_string();
    write_color(&mut stdout, &status, status_color);

    let outcome: String = format!(" {} ms {} {}", duration.as_millis(), body_len, unit);
    writeln(&mut stdout, &outcome);

    let border_len: usize = version.len() + status.len() + outcome.len();
    let border = "━".repeat(border_len);
    writeln(&mut stdout, &border);

    if args.headers {
        let mut spec = ColorSpec::new();
        spec.set_dimmed(true);
        for (k, v) in headers.clone() {
            match k {
                Some(k) => writeln_spec(&mut stdout, &format!("{}: {:?}", k, v), &spec),
                None => log::warn!("Found header key that was empty or unresolvable"),
            }
        }
        if !body.is_empty() {
            io::writeln(&mut stdout, "");
        }
    }

    if !body.is_empty() {
        let content_type = headers.get("content-type").and_then(|ct| ct.to_str().ok());
        let content: String = formatters
            .iter()
            .filter(|fmt| fmt.accept(content_type))
            .fold(body, |content, fmt| fmt.format(content).unwrap());

        io::write(&mut stdout, &content);
        if !content.ends_with('\n') {
            io::writeln(&mut stdout, "");
        }
    }

    Ok(())
}

impl From<SubstitutionError> for FireError {
    fn from(e: SubstitutionError) -> Self {
        match e {
            SubstitutionError::MissingValue(err) => FireError::Template(err),
        }
    }
}
