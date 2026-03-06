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

#![cfg(test)]

use crate::UnixClientBuilder;
use jsonrpsee_core::client::{ClientT, Subscription, SubscriptionClientT};
use jsonrpsee_test_utils::helpers::*;
use jsonrpsee_test_utils::mocks::{Id, StatusCode};
use jsonrpsee_test_utils::TimeoutFutureExt;

#[tokio::test]
async fn method_call_works() {
	let (addr, _handle) = unix_server().await;
	let client = UnixClientBuilder::default().build(&addr).with_default_timeout().await.unwrap().unwrap();
	let response: String = client.request("say_hello", rpc_params![]).with_default_timeout().await.unwrap().unwrap();
	assert_eq!(&response, "hello");
}

#[tokio::test]
async fn notification_works() {
	let (addr, _handle) = unix_server().await;
	let client = UnixClientBuilder::default().build(&addr).with_default_timeout().await.unwrap().unwrap();
	client.notification("say_hello", rpc_params![]).with_default_timeout().await.unwrap().unwrap();
}

#[tokio::test]
async fn batch_request_works() {
	let (addr, _handle) = unix_server().await;
	let client = UnixClientBuilder::default().build(&addr).with_default_timeout().await.unwrap().unwrap();
	let mut batch = vec![];
	batch.push(("say_hello", rpc_params![]));
	batch.push(("say_hello", rpc_params![]));

	let result: Vec<String> = client.batch_request(batch).with_default_timeout().await.unwrap().unwrap();
	assert_eq!(result, vec!["hello".to_string(), "hello".to_string()]);
}
