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
use crate::error::print_error;
use crate::format::ContentFormatter;
use crate::headers::HeaderMap;
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
use std::str::FromStr;
use std::time::Duration;
use std::time::Instant;
use template::SubstitutionError;
use termcolor::{Color, ColorSpec, StandardStream};

fn main() -> Result<(), FireError> {
    let res = exec();
    if let Err(err) = &res {
        print_error(err);
    }
    res
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
    let path = args.file();
    let file = std::fs::read_to_string(path).map_err(|e| io_error_to_fire(e, path))?;
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

    let syntax_highlighting: bool = args.use_colors() != termcolor::ColorChoice::Never;
    let formatters: Vec<Box<dyn ContentFormatter>> = format::formatters(syntax_highlighting);

    let req_headers = request.headers();

    if args.print_request() {
        let title: String = format!("{} {}", request.verb(), request.url().unwrap());
        writeln(&mut stdout, &title);
        let border = "━".repeat(title.len());
        writeln(&mut stdout, &border);

        print_http_reqrep(
            &formatters,
            &mut stdout,
            &request.headers(),
            request.body(),
            args.headers,
        );
    }

    let req = client
        .request(request.verb().into(), request.url().unwrap())
        .timeout(args.timeout())
        .headers(req_headers);

    let req = match request.body() {
        Some(body) => req.body(body.clone()),
        None => req,
    }
    .build()
    .unwrap();

    let start: Instant = Instant::now();
    let resp = client.execute(req).map_err(reqwest_error_to_fire)?;
    let end: Instant = Instant::now();

    let duration: Duration = end.duration_since(start);
    // 8. Print response if successful, or error, if not

    let version = resp.version();
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = resp.text().map_err(|e| FireError::Other(e.to_string()))?;

    log::debug!("Body of response:\n{body}");

    let status_color: Option<Color> = match status.as_u16() {
        200..=299 => Some(Color::Green),
        400..=499 => Some(Color::Yellow),
        500..=599 => Some(Color::Red),
        _ => None,
    };

    let (body_len, unit) = format_size_unit(&body);

    let version: String = format!("{version:?} ");
    write(&mut stdout, &version);

    let status: String = status.to_string();
    write_color(&mut stdout, &status, status_color);

    let outcome: String = format!(" {} ms {} {}", duration.as_millis(), body_len, unit);
    writeln(&mut stdout, &outcome);

    let border_len: usize = version.len() + status.len() + outcome.len();
    let border = "━".repeat(border_len);
    writeln(&mut stdout, &border);

    print_http_reqrep(&formatters, &mut stdout, &headers, &Some(body), args.headers);

    Ok(())
}

fn print_http_reqrep(
    formatters: &[Box<dyn ContentFormatter>],
    stdout: &mut StandardStream,
    hm: &HeaderMap,
    body: &Option<String>,
    should_print_headers: bool,
) {
    let body = body.clone().unwrap_or_default();
    let has_body = !body.is_empty();
    if should_print_headers {
        print_headers(stdout, &hm);
        if has_body {
            io::writeln(stdout, "");
        }
    }
    if has_body {
        let content_type = hm.get("content-type").and_then(|ct| ct.to_str().ok());
        let content = apply_formatting(&formatters, content_type, body);

        io::write(stdout, &content);
        if !content.ends_with('\n') {
            io::writeln(stdout, "");
        }
    }
}

fn format_size_unit(body: &String) -> (usize, &'static str) {
    if body.len() >= 1024 {
        ((body.len() / 1024), "kb")
    } else {
        (body.len(), "b")
    }
}

fn reqwest_error_to_fire(e: reqwest::Error) -> FireError {
    if e.is_timeout() {
        FireError::Timeout(e.url().unwrap().clone())
    } else if e.is_connect() {
        FireError::Connection(e.url().unwrap().clone())
    } else {
        FireError::Other(e.to_string())
    }
}

fn io_error_to_fire<P: AsRef<std::path::Path>>(e: std::io::Error, path: P) -> FireError {
    let path = path.as_ref().to_path_buf();
    match e.kind() {
        std::io::ErrorKind::NotFound => FireError::FileNotFound(path),
        std::io::ErrorKind::PermissionDenied => FireError::NoReadPermission(path),
        _ => FireError::GenericIO(e.to_string()),
    }
}

fn apply_formatting<K: std::string::ToString>(
    formatters: &[Box<dyn ContentFormatter>],
    content_type: Option<&str>,
    body: K,
) -> String {
    let content: String = formatters
        .iter()
        .filter(|fmt| fmt.accept(content_type))
        .fold(body.to_string(), |content, fmt| fmt.format(content).unwrap());
    content
}

fn print_headers(stdout: &mut StandardStream, header_map: &HeaderMap) {
    let mut spec = ColorSpec::new();
    spec.set_dimmed(true);
    for (k, v) in header_map {
        writeln_spec(stdout, &format!("{}: {:?}", k.as_str(), v), &spec);
    }
}

impl From<SubstitutionError> for FireError {
    fn from(e: SubstitutionError) -> Self {
        match e {
            SubstitutionError::MissingValue(err) => FireError::Template(err),
        }
    }
}
