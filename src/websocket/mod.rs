use thiserror::Error;

use base64;
use sha1::Sha1;
use std::io::{Read, Write};
use std::io::{Error, ErrorKind};

use ascii::AsciiString;
use crate::{Request, Header, Response, StatusCode, ReadWrite};

const SUPPORTED_REVISION : u16 = 13;

/// Describes possible error when receiving a
/// websocket connection attempt or trying to
/// deploy the dedicated websocket stream 
#[derive(Error, Debug)]
pub enum WebsocketError {
	#[error("invalid websocket request")]
	InvalidWebsocketRequest,
	#[error("missing `revision number`")]
	MissingRevisionNumber,
	#[error("invalid `revision number` \"{0}\"")]
	InvalidRevision(u16),
	#[error("failed to identify revision number")]
	RevisionParsingError(#[from] std::num::ParseIntError),
	#[error("missing `upgrade` field")]
	MissingUpgradeField,
	#[error("invalid `upgrade` field")]
	InvalidUpgradeField(String),
	#[error("missing `connection` field")]
	MissingConnectionField,
	#[error("invalid `connection` field")]
	InvalidConnectionField(String),
	#[error("missing \"secure\" `key` field")]
	MissingKeyField,
	#[error("missing `protocol` field")]
	MissingProtocolField,
    #[error("data from client is not masked")]
    DataIsNotMasked,
}

/// Websocket handler
pub struct Websocket {
	socket: Box<dyn ReadWrite + Send>,
}

const CONTINUATION_OPCODE 	: u8 = 0x00;
const TEXT_OPCODE 			: u8 = 0x01;
const BINARY_OPCODE			: u8 = 0x02;
const CLOSE_OPCODE			: u8 = 0x08;
const PING_OPCODE 			: u8 = 0x09;
const PONG_OPCODE 			: u8 = 0x0A;

/// Known Websocket frames 
#[derive(Debug)]
pub enum Frame {
	/// Readable text data
	Text(String),
	/// Binary data
	Binary(Vec<u8>),
	/// Socket close request / answer
    /// (code,reason): closing code and possible description about
    /// why we're closing this socket
	Close(Option<u16>, Option<String>),
    /// `Ping` message with optionnal payload
	Ping(Option<String>),
	/// `Pong`: client answering a `Ping`,
    /// client should always copy both http header and ping message
    /// to complete a ping / pong exchange
	Pong(Option<String>),
}

/// Websocket message 
#[derive(Debug)]
pub struct Message {
	/// FIN bit indicates this message
    /// terminates successive frames
	pub fin: bool,
	/// content : actual data 
	pub frame: Frame,
}

impl Default for Frame {
	fn default() -> Frame { Frame::Ping(None) }
}

fn read_bigendian_u16<'a, T: Iterator<Item = &'a u8>>(input: &mut T) -> u16 {
	let buf : [u8; 2] = [
		*input.next().unwrap(), 
		*input.next().unwrap()
	];
	u16::from_be_bytes(buf)
}

fn read_bigendian_u32<'a, T: Iterator<Item = &'a u8>>(input: &mut T) -> u32 {
	let buf : [u8; 4] = [
		*input.next().unwrap(), 
		*input.next().unwrap(),
		*input.next().unwrap(),
		*input.next().unwrap(),
	];
	u32::from_be_bytes(buf)
}

fn read_bigendian_u64<'a, T: Iterator<Item = &'a u8>>(input: &mut T) -> u64 {
	let buf : [u8; 8] = [
		*input.next().unwrap(), 
		*input.next().unwrap(),
		*input.next().unwrap(),
		*input.next().unwrap(),
		*input.next().unwrap(), 
		*input.next().unwrap(),
		*input.next().unwrap(),
		*input.next().unwrap(),
	];
	u64::from_be_bytes(buf)
}

impl Websocket {
	/// Deploys a new websocket channel, from a valid HTTP(s) websocket request.  
    /// This structure can receive and transmit one frame at a time.
	/// request: incoming HTTP(s) request, must match header requirements:
    ///    + has connection upgrade field
    ///    + has websocket upgrade field
    ///    + websocket "version" matches websocket::SUPPORTED_REVISION
	/// protocol: optionnal (sub) protocol to declare   
	pub fn new (request: Request, protocol: Option<&str>) -> Result<Websocket, WebsocketError> {
        let mut upgrade = Response::empty(StatusCode(101)); // `switching` protocol
            /*.with_header(Header {
                field: "Set-Cookie".parse().unwrap(),
                value: AsciiString::from_ascii("SID=abcdefg; Max-Age=3600; Path=/; HttpOnly").unwrap(),
            });*/
        if let Some(protocol) = protocol {
            // add possible desired custom/sub protocol
            if let Some(protocols) = request_protocols(&request) {
                upgrade
                    .add_header(Header{
                        field: "Sec-Websocket-Protocol".parse().unwrap(),
                        value: AsciiString::from_ascii(protocol).unwrap(),
                    });
                /*if !protocols.contains(&protocol.to_string()) {
                    let content = format!("{}, {}",
                        protocol,
                        protocols.join(",")); // put protocols toghether
                    println!("DEBUG: {}", content);
                    upgrade
                        .add_header(Header{
                            field: "Sec-Websocket-Protocol".parse().unwrap(),
                            value: AsciiString::from_ascii(content).unwrap(),
                        });
                } else {
                    println!("Already existing")
                }*/
            } else {
                upgrade
                    .add_header(Header{
                        field: "Sec-Websocket-Protocol".parse().unwrap(),
                        value: AsciiString::from_ascii(protocol).unwrap(),
                    });
            }
        }
        let key = request_key(&request)?; 
        let key = convert_key(&key);
        upgrade
            .add_header(Header{
                field: "Sec-Websocket-Accept".parse().unwrap(),
                value: AsciiString::from_ascii(key).unwrap(),
            });
        let socket = request.upgrade("websocket", upgrade);
        Ok(Websocket {
            socket: socket,
        })
	}
	/// Reads one complete frame from the websocket stream,
    /// this call is blocking
	pub fn recv (&mut self) -> std::io::Result<Message> {
		let mut buf : Vec<u8> = Vec::with_capacity(256);
		if self.socket
            .as_mut()
            .take(2).
            read_to_end(&mut buf).is_err() 
        {
			// not enough bytes
			return Err(Error::new(ErrorKind::UnexpectedEof, "read first two bytes failed"));
		}

		if buf[0] & 0x70 != 0 {
			return Err(Error::new(ErrorKind::Other, "reserved bits (in opcode) must be zero"))
		}
		
		let fin = (buf[0] & 0x80) != 0;
		let opcode = buf[0] & 0x0F;

        let mask = buf[1] & 0x80;
        if mask == 0 {
			return Err(Error::new(ErrorKind::Other, "Client to server messages must be masked"))
		}

        let payload_len = buf[1] & 0x7F;
        let length = match payload_len {
            126 => {
                // the following two bytes interprated as uint16_t
                if self.socket
                    .as_mut()
                    .take(2)
                    .read_to_end(&mut buf).is_err()
                {
        			// not enough bytes
			        return Err(Error::new(ErrorKind::UnexpectedEof, "read 2 bytes failed"))
                }
				payload_len as u64 + read_bigendian_u16(&mut buf.iter()) as u64
            },
            127 => {
                // the following eight bytes interprated as uint64_t
                if self.socket
                    .as_mut()
                    .take(8)
                    .read_to_end(&mut buf).is_err() {
        			// not enough bytes
			        return Err(Error::new(ErrorKind::UnexpectedEof, "read 8 bytes failed"))
                }
				payload_len as u64 + read_bigendian_u64(&mut buf.iter())
            },
            _ => {
                payload_len as u64
            },
        };
        println!("Payload length: {}", length);

        // read mask bits
        if self.socket
            .as_mut()
            .take(4)
            .read_to_end(&mut buf).is_err() {
            return Err(Error::new(ErrorKind::UnexpectedEof, "failed to read mask bits"))
        }
        let mask = read_bigendian_u32(&mut buf.iter().skip(2));
        buf.clear();
        println!("Masking Key: {:x}", mask);
		
		// grab raw data if needed
		let mut data : Option<Vec<u8>> = match opcode {
			BINARY_OPCODE | TEXT_OPCODE => {
				self.socket
					.as_mut()
					.take(length)
					.read_to_end(&mut buf);
				let mut data : Vec<u8> = Vec::with_capacity(length as usize);
				let mut offset : usize = 0;
                // apply binary mask to later interprate correctly
				for i in 0..length as usize {
                    let m = ((mask >> ((3-offset)*8)) & 0xff)  as u8;
					data.push(buf[i] ^ m);
					offset = (offset +1) %4;
				}
				Some(data)
			},
            CLOSE_OPCODE | PONG_OPCODE => {
                if length > 1 { // got at least a termination code
                    // grab payload so we can parse termination code
                    // and possible event description
                    self.socket
                        .as_mut()
                        .take(length)
                        .read_to_end(&mut buf);
                    let mut data : Vec<u8> = Vec::with_capacity(length as usize);
                    let mut offset : usize = 0;
                    // apply binary mask to later interprate correctly
                    for i in 0..length as usize {
                        let m = ((mask >> ((3-offset)*8)) & 0xff)  as u8;
                        data.push(buf[i] ^ m);
                        offset = (offset +1) %4;
                    }
                    Some(data)
                } else {
                    None
                }
            }
			_ => None,
		};

		let frame = match opcode {
			CONTINUATION_OPCODE => {
				panic!("unable to handle continuation opcodes @ the moment")
			},
			BINARY_OPCODE => Frame::Binary(data.unwrap()),
			TEXT_OPCODE => { 
				if let Ok(content) = std::str::from_utf8(&data.unwrap()) {
					Frame::Text(content.to_string())
				} else {
					panic!("text decoding failure")
				}
			},
			CLOSE_OPCODE => {
                let mut code: Option<u16> = None;
                let mut desc: Option<String> = None;
                if let Some(data) = data { // got something to parse
                    if length == 2 {
                        // only got a status code
                        code = Some(read_bigendian_u16(&mut data.iter()))
                    } else {
                        // got status code + event description
                        code = Some(read_bigendian_u16(&mut data.iter()));
                        if let Ok(content) = std::str::from_utf8(&data[2..]) {
                            desc = Some(content.to_string())
                        }
                    }
                }
                Frame::Close(code,desc)
            },
			PING_OPCODE => panic!("Unexpected ping opcode: client must not initiate a ping request"),
			PONG_OPCODE => {
                let mut desc : Option<String> = None;
                if length > 0 {
                    if let Ok(content) = std::str::from_utf8(&data.unwrap()) {
                        desc = Some(content.to_string())
                    }
                }
                Frame::Pong(desc)
			},
            _ => panic!("received non supported opcode")
		};
		Ok(Message{
			fin,
			frame,
		})
	}
    /// Send text data through websocket
    pub fn send_text (&mut self, data: &str) -> std::io::Result<()> {
        self.send_message(Message{
            fin: true,
            frame: Frame::Text(data.to_string())
        })
    }
    /// `Ping` the client with given message payload
    pub fn ping (&mut self, msg: &str) -> std::io::Result<()> {
        self.send_message(Message {
            fin: true,
            frame: Frame::Ping(Some(msg.to_string()))
        })
    }
    /// Send given message through websocket.
    /// It is possible to send all supported Frames but `Pong` 
    /// which is reserved to the client side
    pub fn send_message (&mut self, msg: Message) -> std::io::Result<()> {
        let mut buf : Vec<u8> = Vec::with_capacity(256);
        let mut b0 = 0x00;
        let mut b1 = 0x00;
        if msg.fin {
            b0 |= 0x80; // FIN bit
        }
        let iter = match msg.frame {
            Frame::Text(data) => {
                b0 |= TEXT_OPCODE;
                b1 |= data.len() as u8;
                for b in data.as_str().bytes() {
                    buf.push(b);
                }
            },
            Frame::Binary(data) => {
                b0 |= TEXT_OPCODE;
                b1 |= data.len() as u8;
                for i in 0..data.len() {
                    buf.push(data[i])
                }
            },
            Frame::Close(_,_) => {
                b0 |= CLOSE_OPCODE;
            },
            Frame::Ping(data) => {
                b0 |= PING_OPCODE;
                if let Some(data) = data { // verbose ping
                    b1 |= data.len() as u8;
                    for b in data.as_str().bytes() {
                        buf.push(b);
                    }
                }
            },
            _ => {
                panic!("sending a `pong` is not feasible")
            },
        };
        buf.insert(0,b1);
        buf.insert(0,b0);
        self.socket.write_all(&buf)?;
        self.socket.flush()
    }
}

/// Returns true if given HTTP request header contains special attributes
fn is_connection_upgrade (request: &Request) -> Result<bool, WebsocketError> {
	if let Some(connection) = request
		.headers()
		.iter()
		.find(|h| h.field.equiv(&"Connection"))
		.and_then(|h| Some(h.value.to_string())) {
		Ok(connection.contains("Upgrade"))
	} else {
		Err(WebsocketError::MissingConnectionField)
	}
}

/// Returns true if given HTTP request header contains special attributes
fn is_websocket_upgrade (request: &Request) -> Result<bool, WebsocketError> {
	if let Some(upgrade) = request
		.headers()
		.iter()
		.find(|h| h.field.equiv(&"Upgrade"))
		.and_then(|h| Some(h.value.to_string())) {
		Ok(upgrade.contains("websocket"))
	} else {
		Err(WebsocketError::MissingUpgradeField)
	}
}

/// Returns secure websocket revision
fn websocket_version (request: &Request) -> Result<u16, WebsocketError> {
	if let Some(version) = request
		.headers()
		.iter()
		.find(|h| h.field.equiv("Sec-Websocket-Version"))
		.and_then(|h| Some(h.value.to_string())) {
		Ok(u16::from_str_radix(&version, 10)?)
	} else {
		Err(WebsocketError::MissingRevisionNumber)
	}
}

/// Returns True if websocket revision matches expected value
fn websocket_version_supported (request: &Request) -> Result<bool, WebsocketError> {
	Ok(websocket_version(request)? == SUPPORTED_REVISION)
}

/// Parses websocket key field from HTTP header
fn request_key (request: &Request) -> Result<String, WebsocketError> {
	if let Some(key) = request
		.headers()
		.iter()
		.find(|h| h.field.equiv(&"Sec-Websocket-Key"))
		.and_then(|h| Some(h.value.to_string())) {
		Ok(key)
	} else {
		Err(WebsocketError::MissingKeyField)
	}
}

/// Returns true if given HTTP request matches 
/// a request to open a websocket
pub fn is_websocket_request (request: &Request) -> Result<bool, WebsocketError> {
	Ok(is_connection_upgrade(request)?
		&& is_websocket_upgrade(request)?
		&& websocket_version_supported(request)?)
}

/// Returns list of requested protocols contained from HTTP header
fn request_protocols (request: &Request) -> Option<Vec<String>> {
	if let Some(value) = request
		.headers()	
		.iter()
		.find(|h| h.field.equiv(&"Sec-Websocket-Protocol"))
		.and_then(|h| Some(h.value.to_string()))
	{
		Some(value.split(",")
			.map(str::to_string)
			.collect())
	} else {
		None
	}
}

/// Turns a Sec-Websocket `key` content into a Sec-Websocket `acccept` content
fn convert_key (input: &str) -> String {
	let mut sha1 = Sha1::new();
	sha1.update(input.as_bytes());
	sha1.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
	base64::encode_config(&sha1.digest().bytes(), base64::STANDARD)
}

#[cfg(test)]
mod test {
	use super::*;
	#[test]
	fn test_key_conversion() {
		let key = "Dvwrg+aaihtHnMG8pa91pA==";
		assert_eq!(convert_key(key), String::from("mUlf8nxD+24lYCUApqX/NZBVoLo="));
		let key = "zQFTj8nsUOjDz4AhLA0PrQ==";
		assert_eq!(convert_key(key), String::from("gSTv2Nz9882Y5T3omVJSjLbPwBA="));
	}
}
