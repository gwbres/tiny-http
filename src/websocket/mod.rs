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
/// deploy the dedicated I/O stream
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
pub enum WebsocketFrame {
	/// Readable text data
	Text(String),
	/// Binary data
	Binary(Vec<u8>),
	/// Client requested stream to be closed
	CloseRequest,
	/// Client sent a `Ping` (keep alive) request 
	Ping,
	/// `Pong` is anwser to `Ping` request 
	Pong,
}

/// Websocket message 
#[derive(Debug)]
pub struct Message {
	/// FIN bit indicates this message
    /// terminates successive frames
	pub fin: bool,
	/// content : actual data 
	pub frame: WebsocketFrame,
}

impl Default for WebsocketFrame {
	fn default() -> WebsocketFrame { WebsocketFrame::Pong }
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
	/// Deploys a new websocket handler (Rd/Wr stream) ready to use,
	/// from a valid HTTP(s) request. 
	/// Request should match HTTP header requirements (refer to doc).   
	/// request: received from client
	/// protocol: optionnal (custom sub-) protocol to declare   
	/// If Err() (unmet header requirements),
	/// it is best pratice to respond with a 404 error
	pub fn new (request: Request, protocol: Option<&str>) -> Result<Websocket, WebsocketError> {
		if let Ok(true) = is_websocket_request(&request) {
			let mut upgrade = Response::empty(StatusCode(101)) // `switching` protocol
				.with_header(Header {
					field: "Set-Cookie".parse().unwrap(),
					value: AsciiString::from_ascii("SID=abcdefg; Max-Age=3600; Path=/; HttpOnly").unwrap(),
				});
			if let Some(protocol) = protocol {
				// add possible desired custom protocol
				if let Some(protocols) = request_protocols(&request) {
					upgrade
						.add_header(Header{
							field: "Sec-Websocket-Protocol".parse().unwrap(),
							value: AsciiString::from_ascii(format!("{}", protocol)).unwrap(), //TODO retrieve all previous protocols please
						});
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
		} else {
			Err(WebsocketError::InvalidWebsocketRequest)
		}
	}
	
	/// Read message from the websocket stream, 
    /// blocking call
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
				read_bigendian_u16(&mut buf.iter()) as u64
            },
            127 => {
                // the following eight bytes interprated as uint16_t
                if self.socket
                    .as_mut()
                    .take(8)
                    .read_to_end(&mut buf).is_err() {
        			// not enough bytes
			        return Err(Error::new(ErrorKind::UnexpectedEof, "read 8 bytes failed"))
                }
				read_bigendian_u64(&mut buf.iter())
            },
            _ => payload_len as u64
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
			_ => None,
		};

		let frame = match opcode {
			CONTINUATION_OPCODE => {
				panic!("unable to handle continuation opcodes @ the moment")
			},
			BINARY_OPCODE => WebsocketFrame::Binary(data.unwrap()),
			TEXT_OPCODE => { 
				if let Ok(content) = std::str::from_utf8(&data.unwrap()) {
					WebsocketFrame::Text(content.to_string())
				} else {
					panic!("text decoding failure")
				}
			},
			CLOSE_OPCODE => WebsocketFrame::CloseRequest,
			PING_OPCODE => WebsocketFrame::Ping,
			PONG_OPCODE => {
				panic!("received unexpected pong answer")
			},
			_ => {
				panic!("received unknown opcode {}", opcode)
			}
		};
		Ok(Message{
			fin,
			frame,
		})
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

/// Sends given data as a valid Websocket Frame
fn send_data<W: Write> (data: &[u8], opcode: u8, mut stream: W) -> std::io::Result<()> {
	let opcode = 0x80 | opcode;
	// frame opcode
	stream.write_all(&[opcode])?;
	// frame length
	stream.write_all(&[data.len() as u8])?;
	//stream.write_all(&[127u8])?; // MAX
	stream.write_all(data)?;
	stream.flush()?;
	Ok(())
}

pub fn send_binary<W: Write> (data: &[u8], mut stream: W) -> std::io::Result<()> {
	send_data(data, 0x02, &mut stream)
}

pub fn send_text<W: Write> (data: &str, mut stream: W) -> std::io::Result<()> {
	send_data(data.as_bytes(), 0x01, &mut stream)
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
