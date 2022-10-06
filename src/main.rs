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
use crate::error::Result;
use crate::format::SessionFormatter;
use crate::headers::HeaderMap;
use crate::http::HttpRequest;
use crate::io::{print_border, print_heading, write, write_color, writeln, writeln_spec};
use crate::logger::setup_logging;
use crate::prop::Property;

use clap::Parser;
use error::FireError;
use std::time::Duration;
use std::time::Instant;

use termcolor::{Color, ColorSpec, StandardStream};

fn main() -> Result<()> {
    let res = exec();
    if let Err(err) = &res {
        print_error(err);
    }
    res
}

struct Context {
    pub args: Args,
    pub stream: StandardStream,
    pub props: Vec<Property>,
    pub client: reqwest::blocking::Client,
    pub session_formatter: SessionFormatter,
}

impl Context {
    fn new_for_args(args: Args) -> Self {
        let stream = StandardStream::stdout(args.use_colors());
        let props = args.env().expect("Unable to load env vars");
        let client = reqwest::blocking::Client::new();
        let session_formatter = SessionFormatter::new(&args);
        Self {
            args,
            stream,
            props,
            client,
            session_formatter,
        }
    }
    fn new() -> Self {
        let args = Args::parse();
        Self::new_for_args(args)
    }
}

fn setup(ctx: &mut Context) -> Result<()> {
    setup_logging(ctx.args.verbosity_level);
    log::debug!("Args: {:?}", ctx.args);
    if ctx.args.print_dbg {
        write(&mut ctx.stream, &dbg_info());
    }
    Ok(())
}

fn exec() -> Result<()> {
    let mut ctx = Context::new();
    setup(&mut ctx)?;
    // 2. Read enviroment variables from system environment and extra environments supplied via cli
    // 3. Apply template substitution

    log::debug!("Received properties {:?}", &ctx.props);

    // 4. Parse Validate format of request
    let request = HttpRequest::from_file(ctx.args.file(), &ctx.props)?;
    // 5. Add user-agent header if missing
    // 6. Add content-length header if missing
    // 7. Make (and optionally print) request
    let req_headers = request.headers();

    if ctx.args.print_request() {
        print_heading(&mut ctx.stream, format!("{} {}", request.verb(), request.url().unwrap()));
        print_http_reqrep(&mut ctx, &request.headers(), request.body());
    }

    let req = ctx
        .client
        .request(request.verb().into(), request.url().unwrap())
        .timeout(ctx.args.timeout())
        .headers(req_headers);

    let req = match request.body() {
        Some(body) => req.body(body.clone()),
        None => req,
    }
    .build()
    .unwrap();

    let start: Instant = Instant::now();
    let resp = ctx.client.execute(req).map_err(reqwest_error_to_fire)?;
    let end: Instant = Instant::now();

    let duration: Duration = end.duration_since(start);
    // 8. Print response if successful, or error, if not

    let version = resp.version();
    let headers = resp.headers().clone();
    let status = resp.status();
    let body = resp.text().map_err(|e| FireError::Other(e.to_string()))?;

    log::debug!("Body of response:\n{body}");

    let (body_len, unit) = format_size_unit(&body);

    let version: String = format!("{version:?} ");
    write(&mut ctx.stream, &version);

    write_color(&mut ctx.stream, status.as_str(), status_color(status));

    let outcome: String = format!(" {} ms {} {}", duration.as_millis(), body_len, unit);
    writeln(&mut ctx.stream, &outcome);

    let border_len: usize = version.len() + status.as_str().len() + outcome.len();
    print_border(&mut ctx.stream, border_len);
    print_http_reqrep(&mut ctx, &headers, &Some(body));

    Ok(())
}

fn status_color(status: reqwest::StatusCode) -> Option<Color> {
    if status.is_success() {
        Some(Color::Green)
    } else if status.is_client_error() {
        Some(Color::Yellow)
    } else if status.is_server_error() {
        Some(Color::Red)
    } else {
        None
    }
}

fn print_http_reqrep(ctx: &mut Context, hm: &HeaderMap, body: &Option<String>) {
    let body = body.clone().unwrap_or_default();
    let has_body = !body.is_empty();
    if ctx.args.headers {
        print_headers(&mut ctx.stream, &hm);
        if has_body {
            io::writeln(&mut ctx.stream, "");
        }
    }
    if has_body {
        let content_type = hm.get("content-type").and_then(|ct| ct.to_str().ok());
        let content = ctx.session_formatter.apply(content_type, &body);
        io::write(&mut ctx.stream, &content);
        if !content.ends_with('\n') {
            io::writeln(&mut ctx.stream, "");
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

fn print_headers(stdout: &mut StandardStream, header_map: &HeaderMap) {
    let mut spec = ColorSpec::new();
    spec.set_dimmed(true);
    for (k, v) in header_map {
        writeln_spec(stdout, &format!("{}: {:?}", k.as_str(), v), &spec);
    }
}
