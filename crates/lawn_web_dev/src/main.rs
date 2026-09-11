//! Native build and localhost development server for the browser target.

use std::{
    env,
    error::Error,
    fs::{self, File},
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Component, Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    thread,
    time::Duration,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const WASM_TARGET: &str = "wasm32-unknown-unknown";

#[derive(Debug, PartialEq, Eq)]
struct Options {
    build_only: bool,
    benchmark: bool,
    open: bool,
    port: u16,
    help: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            build_only: false,
            benchmark: false,
            open: true,
            port: 8080,
            help: false,
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let options = parse_options(env::args().skip(1))?;
    if options.help {
        print_help();
        return Ok(());
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let output = build(&root)?;
    if options.build_only {
        return Ok(());
    }
    serve(&output, &options)
}

fn parse_options(args: impl Iterator<Item = String>) -> Result<Options> {
    let mut options = Options::default();
    let mut args = args;
    let mut command_seen = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "build" | "serve" | "bench" if !command_seen => {
                command_seen = true;
                options.build_only = arg == "build";
                options.benchmark = arg == "bench";
            }
            "--no-open" => options.open = false,
            "--port" => {
                options.port = parse_port(&args.next().ok_or("--port requires a port number")?)?;
            }
            "--help" | "-h" | "help" => options.help = true,
            _ if arg.starts_with("--port=") => options.port = parse_port(&arg[7..])?,
            _ => return Err(format!("unknown argument {arg:?}; run `cargo web --help`").into()),
        }
    }
    Ok(options)
}

fn parse_port(value: &str) -> Result<u16> {
    let port = value
        .parse::<u16>()
        .map_err(|_| "--port must be an integer between 1 and 65535")?;
    if port == 0 {
        return Err("--port must be an integer between 1 and 65535".into());
    }
    Ok(port)
}

fn print_help() {
    println!(
        "Lawn Orbit browser development\n\n\
         Usage: cargo web [serve|build|bench] [--no-open] [--port PORT]\n\n\
         cargo web                  Build optimized WASM, serve, and open the browser\n\
         cargo web --no-open        Build and serve without opening a browser\n\
         cargo web --port 8081       Use a different localhost port (default: 8080)\n\
         cargo web build            Build deployable static files in target/web\n\n\
         cargo web bench            Build, serve, and open the fixed-workload benchmark\n\n\
         Requires the wasm32-unknown-unknown Rust target and a wasm-bindgen CLI\n\
         version matching Cargo.lock. The build prints exact setup commands if\n\
         either is missing. A browser with WebGPU is required.\n\n\
         Builds use the release profile. After editing, stop with Ctrl-C and rerun\n\
         cargo web, then refresh the browser. Saves belong to the page's origin;\n\
         keep the same localhost port to retain the same browser save storage."
    );
}

fn build(root: &Path) -> Result<PathBuf> {
    let version = bindgen_version(&fs::read_to_string(root.join("Cargo.lock"))?)?;
    check_bindgen(&version)?;
    check_wasm_target()?;

    // Specify the output directory so the host and generated artifacts have a
    // stable location even when a caller has CARGO_TARGET_DIR configured.
    let target_dir = root.join("target");
    println!("Building optimized WebGPU/WASM target…");
    let status = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .current_dir(root)
        .args([
            "build",
            "--release",
            "--target",
            WASM_TARGET,
            "-p",
            "lawn_orbit",
            "--lib",
        ])
        .arg("--target-dir")
        .arg(&target_dir)
        .status()?;
    if !status.success() {
        return Err("WASM build failed; see Cargo's diagnostics above".into());
    }

    // Cargo may have updated the lockfile during dependency resolution.
    let version = bindgen_version(&fs::read_to_string(root.join("Cargo.lock"))?)?;
    check_bindgen(&version)?;
    let output = target_dir.join("web");
    if output.exists() {
        fs::remove_dir_all(&output)?;
    }
    fs::create_dir_all(&output)?;
    let status = Command::new("wasm-bindgen")
        .arg(target_dir.join(WASM_TARGET).join("release/lawn_orbit.wasm"))
        .args(["--target", "web", "--out-name", "lawn_orbit", "--out-dir"])
        .arg(&output)
        .status()?;
    if !status.success() {
        return Err("wasm-bindgen failed; see its diagnostics above".into());
    }
    copy_assets(&root.join("web"), &output)?;
    copy_assets(
        &root.join("crates/lawn_orbit/assets/fonts"),
        &output.join("fonts"),
    )?;
    if !output.join("index.html").is_file() {
        return Err("web/index.html is missing; browser host assets are incomplete".into());
    }
    println!("Browser files ready at {}", output.display());
    Ok(output)
}

fn bindgen_version(lockfile: &str) -> Result<String> {
    for package in lockfile.split("[[package]]").skip(1) {
        let mut name = None;
        let mut version = None;
        for line in package.lines() {
            if let Some(value) = line.strip_prefix("name = ") {
                name = Some(value.trim_matches('"'));
            }
            if let Some(value) = line.strip_prefix("version = ") {
                version = Some(value.trim_matches('"'));
            }
        }
        if name == Some("wasm-bindgen") {
            return version
                .map(String::from)
                .ok_or_else(|| "wasm-bindgen package in Cargo.lock has no version".into());
        }
    }
    Err("Cargo.lock has no wasm-bindgen package; run `cargo generate-lockfile`".into())
}

fn check_bindgen(expected: &str) -> Result<()> {
    let setup = format!(
        "Install the matching tool with:\n  cargo install wasm-bindgen-cli --version {expected} --locked --force"
    );
    let output = Command::new("wasm-bindgen")
        .arg("--version")
        .output()
        .map_err(|error| format!("cannot run wasm-bindgen: {error}\n{setup}"))?;
    let actual = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() || actual.split_whitespace().nth(1) != Some(expected) {
        return Err(format!(
            "wasm-bindgen CLI must match Cargo.lock ({expected}); found {:?}\n{setup}",
            actual.trim()
        )
        .into());
    }
    Ok(())
}

fn check_wasm_target() -> Result<()> {
    let output = Command::new(env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .args(["--print", "target-libdir", "--target", WASM_TARGET])
        .output()?;
    let libdir = String::from_utf8_lossy(&output.stdout);
    if !output.status.success()
        || !fs::read_dir(libdir.trim()).is_ok_and(|mut entries| entries.next().is_some())
    {
        return Err(format!(
            "the Rust WASM target is not installed. Install it with:\n  rustup target add {WASM_TARGET}"
        )
        .into());
    }
    Ok(())
}

fn copy_assets(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_assets(&entry.path(), &destination.join(entry.file_name()))?;
        } else if kind.is_file() {
            fs::copy(entry.path(), destination.join(entry.file_name()))?;
        } else {
            return Err(format!(
                "web assets must be regular files: {}",
                entry.path().display()
            )
            .into());
        }
    }
    Ok(())
}

fn serve(root: &Path, options: &Options) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", options.port)).map_err(|error| {
        format!(
            "cannot serve port {}: {error}; try `cargo web --port {}`",
            options.port,
            options.port.saturating_add(1)
        )
    })?;
    let path = if options.benchmark {
        "benchmark.html"
    } else {
        ""
    };
    let url = format!("http://127.0.0.1:{}/{path}", listener.local_addr()?.port());
    println!("Serving {url}\nPress Ctrl-C to stop. Rebuild and refresh after editing.");
    if options.open
        && let Err(error) = open_browser(&url)
    {
        eprintln!("Could not open the browser: {error}. Open {url} manually.");
    }
    let root = root.canonicalize()?;
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let root = root.clone();
                thread::spawn(move || {
                    if let Err(error) = handle_request(stream, &root) {
                        eprintln!("HTTP request failed: {error}");
                    }
                });
            }
            Err(error) => eprintln!("HTTP connection failed: {error}"),
        }
    }
    Ok(())
}

fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("rundll32");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = Command::new("xdg-open");
    let mut child = command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // Some Linux openers remain alive until the browser closes. Start serving
    // immediately, and reap the child without blocking HTTP requests.
    let url = url.to_owned();
    thread::spawn(move || {
        if let Ok(status) = child.wait()
            && !status.success()
        {
            eprintln!("Browser opener exited with {status}. Open {url} manually.");
        }
    });
    Ok(())
}

fn handle_request(mut stream: TcpStream, root: &Path) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let mut request = Vec::new();
    let mut buffer = [0; 1024];
    loop {
        let count = stream.read(&mut buffer)?;
        if count == 0 {
            return Ok(());
        }
        request.extend_from_slice(&buffer[..count]);
        if request.len() > 16_384 {
            return send_error(&mut stream, "431 Request Header Fields Too Large", false);
        }
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let Ok(request) = std::str::from_utf8(&request) else {
        return send_error(&mut stream, "400 Bad Request", false);
    };
    let Some(line) = request.lines().next() else {
        return send_error(&mut stream, "400 Bad Request", false);
    };
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    let head = method == "HEAD";
    if method != "GET" && !head {
        return send_error(&mut stream, "405 Method Not Allowed", false);
    }
    if !matches!(parts.next(), Some("HTTP/1.0" | "HTTP/1.1")) || parts.next().is_some() {
        return send_error(&mut stream, "400 Bad Request", head);
    }
    let Ok(relative) = request_path(target) else {
        return send_error(&mut stream, "400 Bad Request", head);
    };
    let Ok(path) = root.join(relative).canonicalize() else {
        return send_error(&mut stream, "404 Not Found", head);
    };
    if !path.starts_with(root) {
        return send_error(&mut stream, "403 Forbidden", head);
    }
    let Ok(mut file) = File::open(&path) else {
        return send_error(&mut stream, "404 Not Found", head);
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return send_error(&mut stream, "404 Not Found", head);
    }
    write_headers(&mut stream, "200 OK", mime_type(&path), metadata.len())?;
    if !head {
        io::copy(&mut file, &mut stream)?;
    }
    Ok(())
}

fn request_path(target: &str) -> std::result::Result<PathBuf, ()> {
    let path = target.split('?').next().ok_or(())?;
    if !path.starts_with('/') || path.starts_with("//") {
        return Err(());
    }
    let mut decoded = Vec::new();
    let mut bytes = path.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = char::from(bytes.next().ok_or(())?).to_digit(16).ok_or(())?;
            let low = char::from(bytes.next().ok_or(())?).to_digit(16).ok_or(())?;
            decoded.push((high * 16 + low) as u8);
        } else {
            decoded.push(byte);
        }
    }
    let decoded = String::from_utf8(decoded).map_err(|_| ())?;
    if decoded.contains(['\\', '\0']) || decoded.starts_with("//") {
        return Err(());
    }
    let relative = &decoded[1..];
    if relative.is_empty() {
        return Ok(PathBuf::from("index.html"));
    }
    let path = PathBuf::from(relative);
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
        || relative.split('/').any(|part| matches!(part, "." | ".."))
        // Reject drive prefixes and alternate data streams on Windows too.
        || relative.contains(':')
    {
        return Err(());
    }
    Ok(path)
}

fn mime_type(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("txt" | "md") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn write_headers(stream: &mut impl Write, status: &str, mime: &str, size: u64) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\n\
         Content-Type: {mime}\r\n\
         Content-Length: {size}\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Connection: close\r\n\r\n"
    )
}

fn send_error(stream: &mut impl Write, status: &str, head: bool) -> io::Result<()> {
    write_headers(
        stream,
        status,
        "text/plain; charset=utf-8",
        status.len() as u64,
    )?;
    if !head {
        stream.write_all(status.as_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn options(args: &[&str]) -> Result<Options> {
        parse_options(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn defaults_launch_browser_and_build_does_not_serve() {
        assert_eq!(options(&[]).unwrap(), Options::default());
        assert!(options(&["build"]).unwrap().build_only);
        assert!(options(&["bench"]).unwrap().benchmark);
        assert!(!options(&["bench"]).unwrap().build_only);
        let serve = options(&["serve", "--no-open", "--port", "9123"]).unwrap();
        assert!(!serve.build_only);
        assert!(!serve.open);
        assert_eq!(serve.port, 9123);
        assert_eq!(options(&["--port=9124"]).unwrap().port, 9124);
    }

    #[test]
    fn invalid_arguments_are_errors() {
        for args in [
            vec!["--port"],
            vec!["--port", "0"],
            vec!["--port", "65536"],
            vec!["--port", "abc"],
            vec!["build", "serve"],
            vec!["--debug"],
        ] {
            assert!(options(&args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn bindgen_version_comes_from_exact_locked_package() {
        let lock = "version = 4\n[[package]]\nname = \"wasm-bindgen-shared\"\nversion = \"0.2.1\"\n\n[[package]]\nname = \"wasm-bindgen\"\nversion = \"0.2.127\"\n";
        assert_eq!(bindgen_version(lock).unwrap(), "0.2.127");
        assert!(bindgen_version("[[package]]\nname = \"other\"").is_err());
    }

    #[test]
    fn paths_accept_assets_and_reject_traversal() {
        assert_eq!(request_path("/?seed=123").unwrap(), Path::new("index.html"));
        assert_eq!(
            request_path("/assets/font%20one.woff2").unwrap(),
            Path::new("assets/font one.woff2")
        );
        for target in [
            "/../Cargo.toml",
            "/%2e%2e/Cargo.toml",
            "/foo/../../Cargo.toml",
            "/foo/./bar",
            "/foo%5c..%5csecret",
            "/%2fetc/passwd",
            "//etc/passwd",
            "/C:/secret",
            "/file%00.txt",
            "/%zz",
            "/%2",
            "/%ff",
            "https://other/",
        ] {
            assert!(request_path(target).is_err(), "{target}");
        }
    }

    #[test]
    fn wasm_and_modules_have_browser_mime_types() {
        assert_eq!(mime_type(Path::new("app.wasm")), "application/wasm");
        assert_eq!(
            mime_type(Path::new("worker.js")),
            "text/javascript; charset=utf-8"
        );
    }

    #[derive(Debug)]
    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let path = env::temp_dir().join(format!(
                "lawn-web-dev-test-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn request(root: &Path, request: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let root = root.to_owned();
        let handler = thread::spawn(move || {
            handle_request(listener.accept().unwrap().0, &root).unwrap();
        });
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        handler.join().unwrap();
        response
    }

    #[test]
    fn http_serves_wasm_head_errors_and_uncached_assets() {
        let directory = TestDirectory::new();
        fs::write(directory.0.join("app.wasm"), "wasm-test").unwrap();
        let get = request(
            &directory.0,
            "GET /app.wasm HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        assert!(get.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(get.contains("Content-Type: application/wasm\r\n"));
        assert!(get.contains("Cache-Control: no-store\r\n"));
        assert!(!get.contains("Cross-Origin-Embedder-Policy"));
        assert!(get.ends_with("\r\n\r\nwasm-test"));
        let head = request(&directory.0, "HEAD /app.wasm HTTP/1.1\r\n\r\n");
        assert!(head.contains("Content-Length: 9\r\n"));
        assert!(head.ends_with("\r\n\r\n"));
        let missing = request(&directory.0, "GET /missing HTTP/1.1\r\n\r\n");
        assert!(missing.starts_with("HTTP/1.1 404 Not Found"));
        let traversal = request(&directory.0, "GET /%2e%2e/Cargo.toml HTTP/1.1\r\n\r\n");
        assert!(traversal.starts_with("HTTP/1.1 400 Bad Request"));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_cannot_escape_server_root() {
        let directory = TestDirectory::new();
        let outside = TestDirectory::new();
        fs::write(outside.0.join("secret"), "secret").unwrap();
        std::os::unix::fs::symlink(outside.0.join("secret"), directory.0.join("escape")).unwrap();
        let response = request(&directory.0, "GET /escape HTTP/1.1\r\n\r\n");
        assert!(response.starts_with("HTTP/1.1 403 Forbidden"));
        assert!(!response.ends_with("secret"));
    }
}
