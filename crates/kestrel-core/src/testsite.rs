// kestrel :: testsite — the built-in integration-test site (std-only HTTP).
/// A minimal test site on a random port: static page, a JS-escalation shell,
/// a set-cookie endpoint. No external server needed.
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

/// A running test site; killing the child on drop.
pub struct Site {
    pub url: String,
}

/// Start the test site on a random port (spawns `ksl __test-site <port>`).
pub fn start() -> Site {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind :0");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || serve_on(listener));
    // wait for the port to accept connections
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Site {
        url: format!("http://127.0.0.1:{port}"),
    }
}

/// Serve the test site forever on a fresh listener (CLI `test-site` command).
pub fn serve_forever(port: u16) {
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind test port");
    serve_on(listener);
}

pub fn serve_on(listener: TcpListener) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let mut buf = String::new();
        let _ = BufReader::new(&stream).read_line(&mut buf);
        let path = buf.split(' ').nth(1).unwrap_or("/").to_string();
        let (status, ctype, body): (&str, &str, String) = match path.as_str() {
            "/" => (
                "200 OK",
                "text/html",
                "<!doctype html><html><head><title>Kestrel Test Site</title><meta name=\"description\" content=\"a page built to exercise kestrel\"></head><body><h1 id=\"main-title\">Welcome to Kestrel</h1><ul id=\"list\"><li class=\"item\">alpha</li><li class=\"item\">beta</li></ul><table><tr><th>n</th></tr><tr><td>7</td></tr></table><a href=\"/page2.html\">Page Two</a><script>fetch('/api/data')</script></body></html>".into(),
            ),
            "/page2.html" => (
                "200 OK",
                "text/html",
                "<!doctype html><html><head><title>Page Two</title></head><body><p id=\"p2\">second</p></body></html>".into(),
            ),
            "/spa" => (
                "200 OK",
                "text/html",
                "<!doctype html><html><head><title>SPA</title></head><body><div id=\"app\"></div><script src=\"/spa.js\"></script></body></html>".into(),
            ),
            "/spa.js" => (
                "200 OK",
                "text/javascript",
                "setTimeout(()=>{document.getElementById('app').innerHTML='<h1>Rendered by JS</h1><p id=\"js-done\">spa content ready</p>'},300);".into(),
            ),
            "/setcookie" => ("200 OK", "text/html", "cookie set".into()),
            "/challenge" => (
                "200 OK",
                "text/html",
                "<!doctype html><html><head><title>Verify</title></head><body><h1>Verify you are human</h1>"
                    .to_owned()
                    + "<div id=\"turnstile-wrapper\" data-sitekey=\"0x4AAAAAAA\">"
                    + "<iframe src=\"/cf-iframe.html\" width=\"300\" height=\"80\"></iframe></div>"
                    + "<input name=\"cf-turnstile-response\" value=\"\">"
                    + "<script>document.addEventListener('click', function(ev){"
                    + "  if (ev.target.closest('.cf-check')) {"
                    + "    document.querySelector('input[name=cf-turnstile-response]').value = 'tok_' + Date.now() + '_xxxxxx';"
                    + "    document.cookie = 'cf_clearance=solved_' + Date.now() + '; path=/';"
                    + "  } });</script></body></html>",
            ),
            "/cf-iframe.html" => (
                "200 OK",
                "text/html",
                ("<!doctype html><html><body style=\"margin:0\"><div class=\"cf-check\" style=\"width:280px;height:60px;background:#faebd7;text-align:center;line-height:60px;cursor:pointer\">Verify checkbox</div>".to_owned()
                    + "<script>document.addEventListener('click', function(){"
                    + "  try { parent.document.querySelector('input[name=cf-turnstile-response]').value = 'tok_' + Date.now() + '_xxxxxx'; } catch (e) {}"
                    + "  document.cookie = 'cf_clearance=solved_' + Date.now() + '; path=/';"
                    + "});</script></body></html>"),
            ),
            _ => ("404 Not Found", "text/html", "nope".into()),
        };
        let mut headers = format!(
            "HTTP/1.1 {status}\r\ncontent-type: {ctype}\r\ncontent-length: {}\r\nconnection: close\r\n",
            body.len()
        );
        if path == "/setcookie" {
            headers.push_str("set-cookie: fromkestrel=yes; Path=/; Max-Age=3600\r\n");
        }
        let _ = stream.write_all(headers.as_bytes());
        let _ = stream.write_all(b"\r\n");
        let _ = stream.write_all(body.as_bytes());
    }
}
