// Copyright 2019-2021 Parity Technologies (UK) Ltd.
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! This example shows how to use the low-level Unix socket API
//! in jsonrpsee.
//!
//! The particular example disconnects connections that
//! make more than ten RPC calls.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use jsonrpsee::core::middleware::{Batch, Notification, RpcServiceBuilder, RpcServiceT};
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::server::unix::call_with_service_builder;
use jsonrpsee::server::{ConnectionGuard, ConnectionState, ServerConfig, ServerHandle, StopHandle, stop_channel};
use jsonrpsee::types::{ErrorObject, ErrorObjectOwned, Id, Request};
use jsonrpsee::{MethodResponse, Methods};
use jsonrpsee_unix_client::UnixClientBuilder;
use tokio::net::UnixListener;
use tokio::sync::mpsc;
use tokio::sync::Mutex as AsyncMutex;
use tracing_subscriber::util::SubscriberInitExt;

/// This is just a counter to limit
/// the number of calls per connection.
/// Once the limit has been exceeded
/// all future calls are rejected.
#[derive(Clone)]
struct CallLimit<S> {
	service: S,
	count: Arc<AsyncMutex<usize>>,
	state: mpsc::Sender<()>,
}

impl<S> RpcServiceT for CallLimit<S>
where
	S: RpcServiceT<
			MethodResponse = MethodResponse,
			BatchResponse = MethodResponse,
			NotificationResponse = MethodResponse,
		> + Send
		+ Sync
		+ Clone
		+ 'static,
{
	type MethodResponse = S::MethodResponse;
	type NotificationResponse = S::NotificationResponse;
	type BatchResponse = S::BatchResponse;

	fn call<'a>(&self, req: Request<'a>) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
		let count = self.count.clone();
		let state = self.state.clone();
		let service = self.service.clone();

		async move {
			let mut lock = count.lock().await;

			if *lock >= 10 {
				let _ = state.try_send(());
				MethodResponse::error(req.id, ErrorObject::borrowed(-32000, "RPC rate limit", None))
			} else {
				let rp = service.call(req).await;
				*lock += 1;
				rp
			}
		}
	}

	fn batch<'a>(&self, batch: Batch<'a>) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
		let count = self.count.clone();
		let state = self.state.clone();
		let service = self.service.clone();

		async move {
			let mut lock = count.lock().await;
			let batch_len = batch.len();

			if *lock >= 10 + batch_len {
				let _ = state.try_send(());
				MethodResponse::error(Id::Null, ErrorObject::borrowed(-32000, "RPC rate limit", None))
			} else {
				let rp = service.batch(batch).await;
				*lock += batch_len;
				rp
			}
		}
	}

	fn notification<'a>(&self, n: Notification<'a>) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
		let count = self.count.clone();
		let service = self.service.clone();

		// A notification is not expected to return a response so the result here doesn't matter
		// rather than other middlewares may not be invoked.
		async move { if *count.lock().await >= 10 { MethodResponse::notification() } else { service.notification(n).await } }
	}
}

#[rpc(server, client)]
pub trait Rpc {
	#[method(name = "say_hello")]
	async fn say_hello(&self) -> Result<String, ErrorObjectOwned>;

	#[method(name = "count")]
	async fn count(&self) -> Result<u32, ErrorObjectOwned>;
}

struct RpcImpl {
	counter: Arc<AtomicU32>,
}

impl RpcServer for RpcImpl {
	async fn say_hello(&self) -> Result<String, ErrorObjectOwned> {
		Ok("Hello from Unix socket!".to_string())
	}

	async fn count(&self) -> Result<u32, ErrorObjectOwned> {
		Ok(self.counter.fetch_add(1, Ordering::Relaxed))
	}
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	let filter = tracing_subscriber::EnvFilter::try_from_default_env()
		.unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
	tracing_subscriber::FmtSubscriber::builder().with_env_filter(filter).finish().try_init()?;

	let socket_path = "/tmp/jsonrpsee-example.sock";

	// Clean up any existing socket file
	let _ = std::fs::remove_file(socket_path);

	let handle = run_server(socket_path).await?;

    let client = UnixClientBuilder::default().build(socket_path).await.unwrap();
    let res = client.say_hello().await.unwrap();
    dbg!(res);
    let res = client.say_hello().await.unwrap();
    dbg!(res);
    

	// Make some test calls using netcat or socat
	tracing::info!("Unix socket server running at: {}", socket_path);
	tracing::info!("Test with: echo '{{\"jsonrpc\":\"2.0\",\"method\":\"say_hello\",\"id\":1}}' | socat - UNIX-CONNECT:{}", socket_path);
	tracing::info!("Or use netstring format: echo '54:{{\"jsonrpc\":\"2.0\",\"method\":\"say_hello\",\"id\":1}},' | socat - UNIX-CONNECT:{}", socket_path);

	// Keep the server running for a while
	tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;

	handle.stop().unwrap();
	handle.stopped().await;

	// Clean up socket file
	let _ = std::fs::remove_file(socket_path);

	Ok(())
}

async fn run_server(socket_path: &str) -> anyhow::Result<ServerHandle> {
	// Create Unix socket listener
	let listener = UnixListener::bind(socket_path)?;

	// Each RPC call/connection get its own `stop_handle`
	// to able to determine whether the server has been stopped or not.
	//
	// To keep the server running the `server_handle`
	// must be kept and it can also be used to stop the server.
	let (stop_handle, server_handle) = stop_channel();

	// This state is cloned for every connection
	// all these types based on Arcs and it should
	// be relatively cheap to clone them.
	//
	// Make sure that nothing expensive is cloned here
	// when doing this or use an `Arc`.
	#[derive(Clone)]
	struct PerConnection {
		methods: Methods,
		stop_handle: StopHandle,
		conn_id: Arc<AtomicU32>,
		conn_guard: ConnectionGuard,
	}

	let rpc_impl = RpcImpl { counter: Arc::new(AtomicU32::new(0)) };

	let per_conn = PerConnection {
		methods: rpc_impl.into_rpc().into(),
		stop_handle: stop_handle.clone(),
		conn_id: Arc::new(AtomicU32::new(0)),
		conn_guard: ConnectionGuard::new(100),
	};

	tokio::spawn(async move {
		loop {
			// The `tokio::select!` macro is used to wait for either of the
			// listeners to accept a new connection or for the server to be
			// stopped.
			let (mut stream, _remote_addr) = tokio::select! {
				res = listener.accept() => {
					match res {
						Ok(conn) => conn,
						Err(e) => {
							tracing::error!("failed to accept Unix socket connection: {:?}", e);
							continue;
						}
					}
				}
				_ = per_conn.stop_handle.clone().shutdown() => break,
			};

			let per_conn = per_conn.clone();

			// Spawn a task to handle each connection
			tokio::spawn(async move {
				let PerConnection { methods, stop_handle, conn_guard, conn_id } = per_conn;

				// jsonrpsee expects a `conn permit` for each connection.
				//
				// This may be omitted if you don't want to limit the number of connections
				// to the server.
				let Some(conn_permit) = conn_guard.try_acquire() else {
					tracing::warn!("Connection limit reached, rejecting connection");
					return;
				};

				let (tx, mut disconnect) = mpsc::channel::<()>(1);
				let rpc_service = RpcServiceBuilder::new().layer_fn(move |service| CallLimit {
					service,
					count: Default::default(),
					state: tx.clone(),
				});

				let conn = ConnectionState::new(stop_handle, conn_id.fetch_add(1, Ordering::Relaxed), conn_permit);
				let server_cfg = ServerConfig::default();

				// Handle the connection, with rate limiting
				tokio::select! {
					// Process the RPC call (handles multiple requests on same connection)
					result = call_with_service_builder(&mut stream, server_cfg, conn, methods, rpc_service) => {
						if let Err(e) = result {
							tracing::error!("Error handling Unix socket connection: {:?}", e);
						}
					}
					// Disconnect if rate limit is exceeded
					_ = disconnect.recv() => {
						tracing::warn!("Rate limit exceeded, closing connection");
					}
				}
			});
		}
	});

	Ok(server_handle)
}
