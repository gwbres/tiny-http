extern crate rustc_serialize;
extern crate sha1;
extern crate tiny_http;

use std::io::Cursor;
use std::thread::spawn;

use tiny_http::{Response, StatusCode};
use tiny_http::websocket;
use tiny_http::websocket::Websocket;
use tiny_http::websocket::is_websocket_request;

fn home_page(port: u16) -> tiny_http::Response<Cursor<Vec<u8>>> {
    tiny_http::Response::from_string(format!(
        "
        <script type=\"text/javascript\">
        var socket = new WebSocket(\"ws://localhost:{0:}/\", \"example\");

        // we use `bytearray` for convenience,
        // when sending binary data to the server
        socket.binaryType = \"arraybuffer\";

        function send_text(data) {{
            socket.send(data);
        }}

        function send_binary(data) {{
            var bytes = []; // char codes
            for (var i =0; i < data.length; ++i) {{
                var byte = data.charCodeAt(i);
                bytes = bytes.concat([byte])
            }}
            var data = new Uint8Array(bytes);
            socket.send(data);
        }}

        socket.onmessage = function(event) {{
            document.getElementById('response').value = event.data;
        }}

        socket.onclose = function() {{
            console.log(\"socket was closed, openning a new one\");
            // re-establish connexion
            socket = new WebSocket(\"ws://localhost:{0:}/\", \"example\");
        }}

        var failure = function() {{
            console.log(\"failure should never happen\");
            console.log(\"server did not respond correctly at some point\");
        }}

        socket.onerror = failure;
        socket.onfailure = failure;

        </script>
        <h2>Websocket duplex channel example</h2>
        <p>Use this entry to send String / UTF8 readable data to the server </p>
        <p>
            <input type=\"text\" id=\"send_text\" />
            <button onclick=\"send_text(document.getElementById('send_text').value)\">Send</button>
        </p>
        
        <p>Use this entry to send raw / binary data</p>
        <p>
            <input type=\"text\" id=\"send_binary\" />
            <button onclick=\"send_binary(document.getElementById('send_binary').value)\">Send</button>
        </p>

        <p>Server is saying : <input type=\"text\" id=\"response\"/></p>
    ",
        port
    ))
    .with_header(
        "Content-type: text/html"
            .parse::<tiny_http::Header>()
            .unwrap(),
    )
}

fn error_code(code: u16) -> Response<std::io::Empty> { Response::empty(StatusCode(code)) }
fn error_404() -> Response<std::io::Empty> { error_code(404) }

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
                                    websocket::Frame::Text(data) => {
                                        println!("Received text data {:#?}", data);
                                        ws.send_text(&format!("You sent \"{}\"", data)).unwrap();
                                    },
                                    websocket::Frame::Binary(data) => {
                                        println!("Received raw data {:#?}", data);
                                        let mut answer : String = String::from("You sent \"");
                                        for byte in data {
                                            answer.push_str(&format!("{}, ", byte));
                                        }
                                        ws.send_text(&answer).unwrap();
                                    },
                                    websocket::Frame::Close => {
                                        println!("Client requested ws termination");
                                        //request.respond(error_404());
                                        ws.send_message(websocket::Message{
                                            fin: true,
                                            frame: websocket::Frame::Close,
                                        }).unwrap();
                                        println!("close channel");
                                        break // terminates websocket
                                    },
                                    websocket::Frame::Ping => {
                                        println!("Received `ping` request");
                                        ws.send_message(websocket::Message{
                                            fin: true,
                                            frame: websocket::Frame::Pong,
                                        }).unwrap()
                                    },
                                    websocket::Frame::Pong => {
                                        println!("Received a `pong` frame, which should never happen")
                                    },
                                }
                            } else {
                                println!("failed to read websocket");
                                println!("websocket termination");
                                break // terminates websocket
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
