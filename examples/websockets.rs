extern crate rustc_serialize;
extern crate sha1;
extern crate tiny_http;

use std::io::Cursor;
use std::thread::spawn;

use tiny_http::websocket::is_websocket_request;
use tiny_http::websocket::{Websocket, WebsocketFrame};

use rustc_serialize::base64::{Config, Newline, Standard, ToBase64};

fn home_page(port: u16) -> tiny_http::Response<Cursor<Vec<u8>>> {
    tiny_http::Response::from_string(format!(
        "
        <script type=\"text/javascript\">
        var socket = new WebSocket(\"ws://localhost:{}/\", \"example\");

        function send(data) {{
            socket.send(data);
        }}

        socket.onmessage = function(event) {{
            document.getElementById('result').innerHTML += event.data + '<br />';
        }}
        </script>
        <p>This example will receive &quot;Hello&quot; for each byte in the packet being sent.
        Tiny-http doesn't support decoding websocket frames, so we can't do anything better.</p>
        <p><input type=\"text\" id=\"msg\" />
        <button onclick=\"send(document.getElementById('msg').value)\">Send</button></p>
        <p>Received: </p>
        <p id=\"result\"></p>
    ",
        port
    ))
    .with_header(
        "Content-type: text/html"
            .parse::<tiny_http::Header>()
            .unwrap(),
    )
}

/// Turns a Sec-WebSocket-Key into a Sec-WebSocket-Accept.
/// Feel free to copy-paste this function, but please use a better error handling.
fn convert_key(input: &str) -> String {
    use sha1::Sha1;

    let mut input = input.to_string().into_bytes();
    let mut bytes = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
        .to_string()
        .into_bytes();
    input.append(&mut bytes);

    let mut sha1 = Sha1::new();
    sha1.update(&input);

    sha1.digest().bytes().to_base64(Config {
        char_set: Standard,
        pad: true,
        line_length: None,
        newline: Newline::LF,
    })
}

fn main() {
    let server = tiny_http::Server::http("0.0.0.0:1234").unwrap();
    let port = server.server_addr().to_ip().unwrap().port();

    println!("Server started");
    println!(
        "To try this example, open a browser to http://localhost:{}/",
        port
    );

    for request in server.incoming_requests() {
        // we are handling this websocket connection in a new task
        spawn(move || {
            if let Ok(true) = is_websocket_request(&request) {
                println!("received websocket opening request");
                if let Ok(mut ws) = Websocket::new(request, Some("example")) {
                    println!("websocket created");
                    spawn(move || { // stream lives from now on
                        loop {
                            if let Ok(msg) = ws.recv() {
                                match msg.frame {
                                    WebsocketFrame::Text(data) => {
                                        println!("Received text data {:#?}", data);
                                        ws.send_text(&format!("You sent \"{}\"", data)).unwrap();
                                    },
                                    WebsocketFrame::Binary(data) => {
                                        println!("Received raw data {:#?}", data)
                                    },
                                    WebsocketFrame::CloseRequest => {
                                        println!("Receive stream close() request");
                                        println!("websocket smooth termination");
                                    },
                                    WebsocketFrame::Ping => {
                                        println!("Received `ping` request")
                                    },
                                    WebsocketFrame::Pong => {
                                        println!("Received unexpected pong() answer")
                                    },
                                }
                            } else {
                                println!("failed to read websocket");
                                println!("websocket termination");
                                break // terminate websocket
                            }
                        }
                    });
                }
            } else {
                // sending the HTML page
                request.respond(home_page(port)).expect("Responded");
            }
        });
    }
}
