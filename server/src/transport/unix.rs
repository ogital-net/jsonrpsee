use crate::{
	BatchRequestConfig, ConnectionState, LOG_TARGET,
	middleware::rpc::{RpcService, RpcServiceCfg},
	server::{ServerConfig, handle_rpc_call},
};
use jsonrpsee_core::{
	BoxError,
	middleware::{RpcServiceBuilder, RpcServiceT},
	server::{MethodResponse, Methods},
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

/// Represents error that can occur when reading from a Unix domain socket.
#[derive(Debug, thiserror::Error)]
pub enum UnixError {
	/// The message was too large.
	#[error("The message was too big")]
	TooLarge,
	/// Malformed request
	#[error("Malformed request")]
	Malformed,
	/// I/O error
	#[error("I/O error: {0}")]
	Io(#[from] std::io::Error),
}

/// Attempt to decode a netstring size prefix from a byte buffer.
///
/// Returns `Some((size, colon_index))` if the buffer contains a valid netstring size prefix,
/// where `size` is the u32 size and `colon_index` is the index of the ':' delimiter.
/// Returns `None` if the buffer doesn't contain a valid netstring prefix.
fn decode_netstring_size(buf: &[u8]) -> Option<(u32, usize)> {
	// Find the position of the first ':' character
	let colon_pos = buf.iter().position(|&b| b == b':')?;

	// Nothing before the colon means invalid netstring
	if colon_pos == 0 {
		return None;
	}

	// Try to parse the bytes before ':' as a decimal u32
	let size_bytes = &buf[..colon_pos];
	let size_str = std::str::from_utf8(size_bytes).ok()?;
	let size = size_str.parse::<u32>().ok()?;

	Some((size, colon_pos))
}

/// Read data from a Unix domain socket.
/// Supports both netstring-framed requests (size:data,) and newline-delimited requests.
///
/// Returns `Ok((bytes, single))` if the body was in valid size range; and a bool indicating whether the JSON-RPC
/// request is a single or a batch.
/// Returns `Err` if the body was too large or the body couldn't be read.
pub async fn read_body<S>(stream: S, max_body_size: u32) -> Result<(Vec<u8>, bool), UnixError>
where
	S: tokio::io::AsyncRead + Unpin,
{
	let mut reader = tokio::io::BufReader::new(stream);

	// Peek up to 11 bytes to check if this is a netstring-framed request
	let buf = reader.fill_buf().await?;
	let len = std::cmp::min(buf.len(), 11); // 4294967295 + ':;

	// Try to decode as netstring
	if let Some((size, colon_idx)) = decode_netstring_size(&buf[..len]) {
		// Netstring format: size:data,
		if size > max_body_size {
			return Err(UnixError::TooLarge);
		}

		// Discard the size prefix and colon
		reader.consume(colon_idx + 1);

		// Read data + trailing comma directly into buffer
		let mut data: Vec<u8> = Vec::with_capacity(size as usize + 1);
		if let Err(e) = reader.read_exact(unsafe { std::mem::transmute(data.spare_capacity_mut()) }).await {
			if e.kind() == std::io::ErrorKind::UnexpectedEof {
				return Err(UnixError::Malformed);
			}
			return Err(UnixError::Io(e));
		}
		// SAFETY: buffer was filled to capacity
		unsafe { data.set_len(data.capacity()) };

		// Verify trailing comma
		if data[size as usize] != b',' {
			return Err(UnixError::Malformed);
		}

		// Truncate to remove trailing comma (avoids copy)
		data.truncate(size as usize);

		let is_single = match data[0] {
			b'{' => true,
			b'[' => false,
			_ => return Err(UnixError::Malformed),
		};

		tracing::trace!(
			target: "jsonrpsee-uds",
			"Unix socket body (netstring): {}",
			std::str::from_utf8(&data).unwrap_or("Invalid UTF-8 data")
		);

		return Ok((data, is_single));
	}

	// Fall back to newline-delimited reading
	let mut limited_reader = reader.take(max_body_size as u64);
	//let mut limited_reader = tokio::io::BufReader::new(limited_reader);
	let mut buffer = Vec::with_capacity(4 * 1024);

	let bytes_read = limited_reader.read_until(b'\n', &mut buffer).await?;

	if bytes_read == 0 {
		return Err(UnixError::Malformed);
	}

	// Check if we hit the size limit without finding a newline
	// If we read exactly max_body_size and the last byte is not '\n', the message is too large
	if bytes_read as u32 == max_body_size && buffer.last() != Some(&b'\n') {
		return Err(UnixError::TooLarge);
	}

	// Remove trailing newline if present
	if buffer.last() == Some(&b'\n') {
		buffer.truncate(buffer.len() - 1);
	}

	// Determine if this is a single request or batch by checking the first non-whitespace character
	let first_non_whitespace = buffer.iter().find(|byte| !byte.is_ascii_whitespace());

	let is_single = match first_non_whitespace {
		Some(b'{') => true,
		Some(b'[') => false,
		_ => return Err(UnixError::Malformed),
	};

	tracing::trace!(
		target: "jsonrpsee-uds",
		"Unix socket body (newline): {}",
		std::str::from_utf8(&buffer).unwrap_or("Invalid UTF-8 data")
	);

	Ok((buffer, is_single))
}

/// Make JSON-RPC Unix domain socket call with a [`RpcServiceBuilder`]
///
/// Fails if the request was a malformed JSON-RPC request.
#[allow(dead_code)]
pub async fn call_with_service_builder<L>(
	connection: &mut tokio::net::UnixStream,
	server_cfg: ServerConfig,
	conn: ConnectionState,
	methods: impl Into<Methods>,
	rpc_service: RpcServiceBuilder<L>,
) -> Result<(), BoxError>
where
	L: tower::Layer<RpcService>,
	<L as tower::Layer<RpcService>>::Service: RpcServiceT<
			MethodResponse = MethodResponse,
			BatchResponse = MethodResponse,
			NotificationResponse = MethodResponse,
		> + Send,
{
	let ServerConfig { max_response_body_size, batch_requests_config, max_request_body_size, .. } = server_cfg;

	let rpc_service = rpc_service.service(RpcService::new(
		methods.into(),
		max_response_body_size as usize,
		conn.conn_id.into(),
		RpcServiceCfg::OnlyCalls,
	));

	let rp = call_with_service(connection, batch_requests_config, max_request_body_size, rpc_service).await;

	drop(conn);

	connection.write_all(rp.as_bytes()).await?;

	Ok(())
}

/// Make JSON-RPC Unix domain socket call with a [`RpcServiceBuilder`]
///
/// Fails if the request was a malformed JSON-RPC request.
pub async fn call_with_service<S>(
	connection: &mut tokio::net::UnixStream,
	batch_config: BatchRequestConfig,
	max_request_size: u32,
	rpc_service: S,
) -> String
where
	S: RpcServiceT<
			MethodResponse = MethodResponse,
			BatchResponse = MethodResponse,
			NotificationResponse = MethodResponse,
		> + Send,
{
	let (body, is_single) = match read_body(connection, max_request_size).await {
		Ok(r) => r,
		Err(UnixError::TooLarge) => return response::too_large(max_request_size),
		Err(UnixError::Malformed) => return response::malformed(),
		Err(UnixError::Io(e)) => {
			tracing::warn!(target: LOG_TARGET, "Internal error reading request body: {}", e);
			return response::internal_error();
		}
	};

	let rp = handle_rpc_call(&body, is_single, batch_config, &rpc_service, http::Extensions::new()).await;
	response::from_method_response(rp)
}

/// Unix response helpers.
pub mod response {
	use jsonrpsee_core::server::MethodResponse;
	use jsonrpsee_types::error::{ErrorCode, reject_too_big_request};
	use jsonrpsee_types::{ErrorObjectOwned, Id, Response, ResponsePayload};

	/// Create a response for json internal error.
	pub fn internal_error() -> String {
		let err = ResponsePayload::<()>::error(ErrorObjectOwned::from(ErrorCode::InternalError));
		let rp = Response::new(err, Id::Null);
		serde_json::to_string(&rp).expect("built from known-good data; qed")
	}

	/// Create a json response for oversized requests
	pub fn too_large(limit: u32) -> String {
		let err = ResponsePayload::<()>::error(reject_too_big_request(limit));
		let rp = Response::new(err, Id::Null);
		serde_json::to_string(&rp).expect("JSON serialization infallible; qed")
	}

	/// Create a json response for empty or malformed requests
	pub fn malformed() -> String {
		let rp = Response::new(ResponsePayload::<()>::error(ErrorCode::ParseError), Id::Null);
		serde_json::to_string(&rp).expect("JSON serialization infallible; qed")
	}

	/// Create a response from a method response.
	///
	/// This will include the body and extensions from the method response.
	pub fn from_method_response(rp: MethodResponse) -> String {
		let (body, _, _) = rp.into_parts();
		String::from(Box::<str>::from(body))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_decode_netstring_size_valid() {
		// Valid netstring prefix
		assert_eq!(decode_netstring_size(b"10:"), Some((10, 2)));
		assert_eq!(decode_netstring_size(b"123:data"), Some((123, 3)));
		assert_eq!(decode_netstring_size(b"0:"), Some((0, 1)));
		assert_eq!(decode_netstring_size(b"4294967295:"), Some((4_294_967_295, 10)));
	}

	#[test]
	fn test_decode_netstring_size_invalid() {
		// No colon
		assert_eq!(decode_netstring_size(b"123"), None);

		// Empty before colon
		assert_eq!(decode_netstring_size(b":data"), None);

		// Non-numeric
		assert_eq!(decode_netstring_size(b"abc:data"), None);

		// Negative number
		assert_eq!(decode_netstring_size(b"-10:data"), None);

		// Overflow u32
		assert_eq!(decode_netstring_size(b"4294967296:data"), None);

		// Invalid UTF-8
		assert_eq!(decode_netstring_size(&[0xFF, 0xFF, b':']), None);

		// Empty buffer
		assert_eq!(decode_netstring_size(b""), None);
	}

	#[tokio::test]
	async fn test_read_body_netstring_single() {
		let json = r#"{"jsonrpc":"2.0","method":"test","id":1}"#;
		let netstring = format!("{}:{},", json.len(), json);

		let (mut client, server) = tokio::io::duplex(1024);
		tokio::spawn(async move {
			use tokio::io::AsyncWriteExt;
			client.write_all(netstring.as_bytes()).await.unwrap();
		});

		let result = read_body(server, 1024).await.unwrap();
		assert_eq!(result.0, json.as_bytes());
		assert_eq!(result.1, true); // is_single
	}

	#[tokio::test]
	async fn test_read_body_netstring_batch() {
		let json = r#"[{"jsonrpc":"2.0","method":"test","id":1}]"#;
		let netstring = format!("{}:{},", json.len(), json);

		let (mut client, server) = tokio::io::duplex(1024);
		tokio::spawn(async move {
			use tokio::io::AsyncWriteExt;
			client.write_all(netstring.as_bytes()).await.unwrap();
		});

		let result = read_body(server, 1024).await.unwrap();
		assert_eq!(result.0, json.as_bytes());
		assert_eq!(result.1, false); // is_batch
	}

	#[tokio::test]
	async fn test_read_body_netstring_too_large() {
		let netstring = b"9999:data,";

		let (mut client, server) = tokio::io::duplex(1024);
		tokio::spawn(async move {
			use tokio::io::AsyncWriteExt;
			client.write_all(netstring).await.unwrap();
		});

		let result = read_body(server, 100).await;
		assert!(matches!(result, Err(UnixError::TooLarge)));
	}

	#[tokio::test]
	async fn test_read_body_netstring_missing_trailing_comma() {
		let json = r#"{"jsonrpc":"2.0","method":"test","id":1}"#;
		let netstring = format!("{}:{}", json.len(), json);

		let (mut client, server) = tokio::io::duplex(1024);
		tokio::spawn(async move {
			use tokio::io::AsyncWriteExt;
			client.write_all(netstring.as_bytes()).await.unwrap();
		});

		let result = read_body(server, 1024).await;
		assert!(matches!(result, Err(UnixError::Malformed)));
	}

	#[tokio::test]
	async fn test_read_body_newline_single() {
		let json = r#"{"jsonrpc":"2.0","method":"test","id":1}"#;
		let data = format!("{}\n", json);

		let (mut client, server) = tokio::io::duplex(1024);
		tokio::spawn(async move {
			use tokio::io::AsyncWriteExt;
			client.write_all(data.as_bytes()).await.unwrap();
		});

		let result = read_body(server, 1024).await.unwrap();
		assert_eq!(result.0, json.as_bytes());
		assert_eq!(result.1, true); // is_single
	}

	#[tokio::test]
	async fn test_read_body_newline_batch() {
		let json = r#"[{"jsonrpc":"2.0","method":"test","id":1}]"#;
		let data = format!("{}\n", json);

		let (mut client, server) = tokio::io::duplex(1024);
		tokio::spawn(async move {
			use tokio::io::AsyncWriteExt;
			client.write_all(data.as_bytes()).await.unwrap();
		});

		let result = read_body(server, 1024).await.unwrap();
		assert_eq!(result.0, json.as_bytes());
		assert_eq!(result.1, false); // is_batch
	}

	#[tokio::test]
	async fn test_read_body_newline_too_large() {
		let json = "x".repeat(200);
		let data = format!("{}\n", json);

		let (mut client, server) = tokio::io::duplex(1024);
		tokio::spawn(async move {
			use tokio::io::AsyncWriteExt;
			client.write_all(data.as_bytes()).await.unwrap();
		});

		let result = read_body(server, 100).await;
		assert!(matches!(result, Err(UnixError::TooLarge)));
	}

	#[tokio::test]
	async fn test_read_body_malformed_json() {
		let data = b"not json\n";

		let (mut client, server) = tokio::io::duplex(1024);
		tokio::spawn(async move {
			use tokio::io::AsyncWriteExt;
			client.write_all(data).await.unwrap();
		});

		let result = read_body(server, 1024).await;
		assert!(matches!(result, Err(UnixError::Malformed)));
	}

	#[tokio::test]
	async fn test_read_body_empty() {
		let (client, server) = tokio::io::duplex(1024);
		drop(client); // Close the writer side

		let result = read_body(server, 1024).await;
		assert!(matches!(result, Err(UnixError::Malformed)));
	}

	#[tokio::test]
	async fn test_read_body_with_whitespace() {
		let json = r#"  {"jsonrpc":"2.0","method":"test","id":1}  "#;
		let data = format!("{}\n", json);

		let (mut client, server) = tokio::io::duplex(1024);
		tokio::spawn(async move {
			use tokio::io::AsyncWriteExt;
			client.write_all(data.as_bytes()).await.unwrap();
		});

		let result = read_body(server, 1024).await.unwrap();
		assert_eq!(result.1, true); // is_single (should skip leading whitespace)
	}
}
