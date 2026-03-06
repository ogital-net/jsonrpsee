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

//! Example demonstrating JSON-RPC Unix domain socket client connecting to a Unix socket server.

use jsonrpsee::core::client::ClientT;
use jsonrpsee::rpc_params;
use jsonrpsee_unix_client::UnixClientBuilder;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	// Setup logging
	tracing_subscriber::fmt()
		.with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".parse().unwrap()))
		.init();

	// Path to the Unix socket - this should match the server's socket path
	let socket_path = "/tmp/jsonrpsee-example.sock";

	println!("Connecting to Unix socket at: {}\n", socket_path);

	// Test with a persistent connection - multiple requests on the same client
	{
		println!("Testing persistent connection (multiple requests on same connection):");
		let client = UnixClientBuilder::default().build(socket_path).await?;

		// Make multiple requests on the same connection
		let response: String = client.request("say_hello", rpc_params![]).await?;
		println!("  1. say_hello: {}", response);

		let count: u32 = client.request("count", rpc_params![]).await?;
		println!("  2. count: {}", count);

		let count: u32 = client.request("count", rpc_params![]).await?;
		println!("  3. count: {}", count);

		let response: String = client.request("say_hello", rpc_params![]).await?;
		println!("  4. say_hello: {}", response);

		let count: u32 = client.request("count", rpc_params![]).await?;
		println!("  5. count: {}", count);

		println!("  ✓ All requests succeeded on the same connection!\n");
	}

	// Test with separate connections
	println!("Testing separate connections:");
	for i in 0..3 {
		let client = UnixClientBuilder::default().build(socket_path).await?;
		let count: u32 = client.request("count", rpc_params![]).await?;
		println!("  Connection {}: count = {}", i + 1, count);
	}

	println!("\nAll tests completed successfully!");

	Ok(())
}
