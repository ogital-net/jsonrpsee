use crate::{
	ConnectionState,
	middleware::rpc::RpcService,
	server::ServerConfig,
};
use jsonrpsee_core::{
	BoxError,
	middleware::{RpcServiceBuilder, RpcServiceT},
	server::{MethodResponse, Methods},
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt};

/// Represents error that can occur when reading from a Unix domain socket.
#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
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

/// Read data from a Unix domain socket until the first newline.
///
/// Returns `Ok((bytes, single))` if the body was in valid size range; and a bool indicating whether the JSON-RPC
/// request is a single or a batch.
/// Returns `Err` if the body was too large or the body couldn't be read.
#[allow(dead_code)]
pub async fn read_body<S>(
	stream: S,
	max_body_size: u32,
) -> Result<(Vec<u8>, bool), UnixError>
where
	S: tokio::io::AsyncRead + Unpin,
{
	let reader = tokio::io::BufReader::new(stream);
	let limited_reader = reader.take(max_body_size as u64);
	let mut limited_reader = tokio::io::BufReader::new(limited_reader);
	let mut buffer = Vec::with_capacity(4 * 1024);
	
	let bytes_read = limited_reader.read_until(b'\n', &mut buffer).await?;
	
	if bytes_read == 0 {
		return Err(UnixError::Malformed);
	}
	
	if bytes_read > max_body_size as usize {
		return Err(UnixError::TooLarge);
	}
	
	// Determine if this is a single request or batch by checking the first non-whitespace character
	let first_non_whitespace = buffer
		.iter()
		.find(|byte| !byte.is_ascii_whitespace());
	
	let is_single = match first_non_whitespace {
		Some(b'{') => true,
		Some(b'[') => false,
		_ => return Err(UnixError::Malformed),
	};
	
	tracing::trace!(
		target: "jsonrpsee-uds",
		"Unix socket body: {}",
		std::str::from_utf8(&buffer).unwrap_or("Invalid UTF-8 data")
	);
	
	Ok((buffer, is_single))
}


/// Make JSON-RPC Unix domain socket call with a [`RpcServiceBuilder`]
///
/// Fails if the request was a malformed JSON-RPC request.
#[allow(dead_code)]
pub async fn call_with_service_builder<L>(
	_connection: tokio::net::UnixStream,
	_server_cfg: ServerConfig,
	_conn: ConnectionState,
	_methods: impl Into<Methods>,
	_rpc_service: RpcServiceBuilder<L>,
) -> Result<(), BoxError>
where
	L: tower::Layer<RpcService>,
	<L as tower::Layer<RpcService>>::Service: RpcServiceT<
			MethodResponse = MethodResponse,
			BatchResponse = MethodResponse,
			NotificationResponse = MethodResponse,
		> + Send,
{
	todo!()
}


