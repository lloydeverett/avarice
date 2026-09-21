// Derived from Astra <https://github.com/ArkForgeLabs/Astra> (version 0.51.2, commit
// 885586cca0ef065ac80d6a7c702d05e60fbdbb47), src/components/http/client/userdata.rs.
// Copyright 2024 ArkForge LLC, licensed under the Apache License, Version 2.0. See LICENSE in this
// crate's root.
//
// Changes from the original:
//   - Removed the `execute_websocket` method from the `UserData` impl of `HTTPClientRequest`, and
//     the `use reqwest_websocket::Upgrade;` that it needed: the WebSocket client is not taken, and
//     `AstraWebSocket` does not satisfy mlua 0.12's `Sync` bound on userdata under its `send`
//     feature.
//   - Astra's `src/components/http/client/websocket.rs` (`AstraWebSocket`) is not in this crate for
//     the same reason; see `client/mod.rs`.
//   - Added a `__tostring` metamethod to `HTTPClientRequest` and `HTTPClientResponse`, and
//     `MetaMethod` to the `mlua` import for it. Astra has none, so `tostring` and `print` give a
//     userdata's type name and its address. Now `tostring` gives `HTTPClientRequest(<method>
//     <url>)` and `HTTPClientResponse(<status code> <url>)`. Headers and bodies are left out on
//     purpose: a request's headers are where a token lives, and a body can be any size. Nothing
//     Astra does is altered: these are additions.
//   - Everything else is unchanged.

use crate::components::AstraBuffer;
use futures::StreamExt;
use mlua::{ExternalError, MetaMethod, UserData};
use std::collections::HashMap;

impl UserData for super::HTTPClientRequest {
    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("HTTPClientRequest({} {})", this.method, this.url))
        });
        methods.add_method("set_method", |_, this, method: String| {
            let mut request = this.clone();
            request.method = method;
            Ok(request)
        });
        methods.add_method_mut("set_header", |_, this, (key, value): (String, String)| {
            let mut request = this.clone();
            request.headers.insert(key, value);
            Ok(request)
        });
        methods.add_method_mut(
            "set_headers",
            |_, this, headers: HashMap<String, String>| {
                let mut request = this.clone();
                request.headers = headers;
                Ok(request)
            },
        );
        methods.add_method_mut("set_forms", |_, _, _body: mlua::Value| {
            panic!("set_forms is deprecated, moved to set_form.");
            #[allow(unreachable_code)]
            Ok(())
        });
        methods.add_method_mut("set_form", |_, this, form: HashMap<String, String>| {
            let mut request = this.clone();
            request.form = form;
            Ok(request)
        });
        methods.add_method_mut("set_body", |lua, this, body: mlua::Value| {
            let mut request = this.clone();
            request.body = Self::body_parser(lua, &mut request.headers, body)?;
            if !request.headers.contains_key("Content-Type") {
                request
                    .headers
                    .insert("Content-Type".to_string(), "text/plain".to_string());
            }
            Ok(request)
        });
        methods.add_method_mut("set_bytes", |_, _, _body: mlua::Value| {
            panic!("set_bytes is deprecated, use the set_body instead.");
            #[allow(unreachable_code)]
            Ok(())
        });
        methods.add_method_mut("set_json", |_, _, _body: mlua::Value| {
            panic!("set_json is deprecated, use the set_body instead.");
            #[allow(unreachable_code)]
            Ok(())
        });
        methods.add_method_mut("set_file", |_, this, file_path: mlua::Value| {
            let mut request = this.clone();
            request.file = Some(file_path);
            Ok(request)
        });
        methods.add_async_method("execute", |_, this, ()| async move {
            let request = this.request_builder().await?;
            match request.send().await {
                Ok(response) => Ok(Self::response_to_http_client_response(response).await),
                Err(e) => Err(e.into_lua_err()),
            }
        });
        methods.add_method("execute_task", |_, _, _: ()| {
            panic!("execute_task is deprecated, use execute within async task instead.");
            #[allow(unreachable_code)]
            Ok(())
        });
        methods.add_async_method(
            "execute_streaming",
            |_, this, callback: mlua::Function| async move {
                tokio::spawn(async move {
                    let request = this.request_builder().await?;
                    let response = match request.send().await {
                        Ok(response) => response,
                        Err(e) => {
                            tracing::error!("HTTP Request did not execute successfully: {e}");
                            return mlua::Result::Ok(());
                        }
                    };

                    // Create initial response with headers
                    let headers = response
                        .headers()
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_string()))
                        .collect();

                    let initial_response = HTTPClientResponse {
                        url: response.url().to_string(),
                        status_code: response.status().as_u16(),
                        remote_address: response.remote_addr().map(|i| i.to_string()),
                        body: AstraBuffer::new(bytes::Bytes::new()),
                        headers,
                    };

                    // Initial callback
                    if let Err(e) = callback.call::<()>(initial_response.clone()) {
                        tracing::error!("Error running initial callback: {e}");
                        return Ok(());
                    }

                    // Process chunks
                    let mut stream = response.bytes_stream();
                    while let Some(chunk) = stream.next().await {
                        match chunk {
                            Ok(chunk) => {
                                let mut chunk_response = initial_response.clone();
                                chunk_response.body = AstraBuffer::new(chunk);
                                if let Err(e) = callback.call::<()>(chunk_response) {
                                    tracing::error!("Error running chunk callback: {e}");
                                    break;
                                }
                            }
                            Err(e) => {
                                tracing::error!("Error receiving chunk: {e}");
                                break;
                            }
                        }
                    }

                    Ok(())
                });
                Ok(())
            },
        );
    }
}

#[derive(Debug, Clone)]
pub struct HTTPClientResponse {
    pub url: String,
    pub status_code: u16,
    pub remote_address: Option<String>,
    pub body: AstraBuffer,
    pub headers: HashMap<String, String>,
}

impl UserData for HTTPClientResponse {
    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!(
                "HTTPClientResponse({} {})",
                this.status_code, this.url
            ))
        });
        methods.add_method("url", |_, this, ()| Ok(this.url.clone()));
        methods.add_method("status_code", |_, this, ()| Ok(this.status_code));
        methods.add_method("remote_address", |_, this, ()| {
            Ok(this.remote_address.clone())
        });
        methods.add_method("body", |_, this, ()| Ok(this.body.clone()));
        methods.add_method("headers", |_, this, ()| Ok(this.headers.clone()));
    }
}
