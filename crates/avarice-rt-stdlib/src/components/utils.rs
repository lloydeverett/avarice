// Derived from Astra <https://github.com/ArkForgeLabs/Astra>, src/components/utils.rs
// Copyright 2024 ArkForge LLC, licensed under the Apache License, Version 2.0.
// See LICENSE and NOTICE in this crate's root.
//
// Changes from the original:
//   - Removed `close_dbs` (`astra_internal__close_all_databases`) and the `DATABASE_POOLS` import: the database component is not taken.
//   - Removed `dotenv_function` (`astra_internal__dotenv_load`): `dotenvy` is not taken.
//   - Removed `pprint`, which replaced Lua's global `print`: `print` belongs to avarice-rt's core.
//   - Removed `setenv` (`astra_internal__setenv`): it wraps `std::env::set_var`, which is unsound in a process with threads.
//   - Removed `invalidate_cache` (`astra_internal__invalidate_cache`): it clears the import cache of Astra's `import.rs`, which is not taken.
//   - Everything else, including `tokio::spawn` for tasks, is unchanged.
//   - Respelled `mlua::SerializeOptions` as `mlua::serde::SerializeOptions`: mlua 0.12, which this workspace is on, moved it; Astra is on 0.11.

use mlua::{LuaSerdeExt, UserData};

pub fn register_to_lua(lua: &mlua::Lua) -> mlua::Result<()> {
    AstraRegex::register_to_lua(lua)?;
    uuid_v4(lua)?;
    // env
    getenv(lua)?;
    // async tasks
    spawn_task(lua)?;
    spawn_interval(lua)?;
    spawn_timeout(lua)?;

    Ok(())
}

pub fn getenv(lua: &mlua::Lua) -> mlua::Result<()> {
    lua.globals().set(
        "astra_internal__getenv",
        lua.create_function(|lua, key: String| {
            if let Ok(value) = std::env::var(key) {
                lua.to_value_with(
                    &value,
                    mlua::serde::SerializeOptions::new()
                        .serialize_none_to_null(false)
                        .serialize_unit_to_null(false),
                )
            } else {
                Ok(mlua::Value::Nil)
            }
        })?,
    )
}

pub fn uuid_v4(lua: &mlua::Lua) -> mlua::Result<()> {
    lua.globals().set(
        "astra_internal__uuid",
        lua.create_function(|lua, _: ()| {
            lua.to_value_with(
                &uuid::Uuid::new_v4(),
                mlua::serde::SerializeOptions::new()
                    .serialize_none_to_null(false)
                    .serialize_unit_to_null(false),
            )
        })?,
    )
}

pub struct TaskHandler<T: Send + 'static> {
    pub handler: Option<tokio::task::JoinHandle<T>>,
}
impl<T: Send + 'static> UserData for TaskHandler<T> {
    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method_mut("abort", |_, this, ()| {
            let handler = this.handler.take();
            if let Some(handler) = handler {
                handler.abort();
            }
            Ok(())
        });

        methods.add_async_method_mut("await", |_, mut this, ()| async move {
            let handler = this.handler.take();
            if let Some(handler) = handler {
                // TODO: Handle the return
                let _ = handler.await;
            }
            Ok(())
        });
    }
}

fn create_async_function<F, T>(function: F) -> TaskHandler<T>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let handle = tokio::spawn(function);
    TaskHandler {
        handler: Some(handle),
    }
}

fn spawn_task(lua: &mlua::Lua) -> mlua::Result<()> {
    lua.globals().set(
        "astra_internal__spawn_task",
        lua.create_async_function(|lua, callback: mlua::Function| async move {
            let registry_key = lua.create_registry_value(callback)?;
            Ok(create_async_function(async move {
                match lua.registry_value::<mlua::Function>(&registry_key) {
                    Ok(callback) => {
                        if let Err(e) = callback.call_async::<()>(()).await {
                            println!("Error running a task: {e}");
                        }
                    }
                    Err(e) => println!("Error getting the task from registry: {e}"),
                }
            }))
        })?,
    )
}

fn spawn_timeout(lua: &mlua::Lua) -> mlua::Result<()> {
    lua.globals().set(
        "astra_internal__spawn_timeout",
        lua.create_async_function(
            |lua, (callback, sleep_length): (mlua::Function, u64)| async move {
                let registry_key = lua.create_registry_value(callback)?;

                Ok(create_async_function(async move {
                    // sleep
                    tokio::time::sleep(std::time::Duration::from_millis(sleep_length)).await;

                    match lua.registry_value::<mlua::Function>(&registry_key) {
                        Ok(callback) => {
                            if let Err(e) = callback.call_async::<()>(()).await {
                                println!("Error running a task: {e}");
                            }
                        }
                        Err(e) => println!("Error getting the task from registry: {e}"),
                    }
                }))
            },
        )?,
    )
}

fn spawn_interval(lua: &mlua::Lua) -> mlua::Result<()> {
    lua.globals().set(
        "astra_internal__spawn_interval",
        lua.create_async_function(
            |lua, (callback, sleep_length): (mlua::Function, u64)| async move {
                let registry_key = lua.create_registry_value(callback)?;

                Ok(create_async_function(async move {
                    loop {
                        match lua.registry_value::<mlua::Function>(&registry_key) {
                            Ok(callback) => {
                                if let Err(e) = callback.call_async::<()>(()).await {
                                    println!("Error running a task: {e}");
                                }
                            }
                            Err(e) => println!("Error getting the task from registry: {e}"),
                        }

                        // sleep
                        tokio::time::sleep(std::time::Duration::from_millis(sleep_length)).await;
                    }
                }))
            },
        )?,
    )
}

pub struct AstraRegex {
    re: regex::Regex,
}
impl AstraRegex {
    pub fn register_to_lua(lua: &mlua::Lua) -> mlua::Result<()> {
        let function = lua.create_function(|_, regex_string: String| {
            match regex::Regex::new(&regex_string) {
                Ok(re) => Ok(Self { re }),
                Err(e) => Err(mlua::Error::runtime(format!(
                    "Could not compile the regex: {e}"
                ))),
            }
        })?;
        lua.globals().set("astra_internal__regex", function)?;

        Ok(())
    }
}
impl mlua::UserData for AstraRegex {
    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("captures", |_, this, content: String| {
            let captures = this
                .re
                .captures_iter(&content)
                .map(|capture| {
                    capture
                        .iter()
                        .filter_map(|content| content.map(|content| content.as_str().to_string()))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();

            Ok(captures)
        });

        methods.add_method("is_match", |_, this, content: String| {
            Ok(this.re.is_match(&content))
        });

        methods.add_method(
            "replace",
            |_, this, (content, replace, limit): (String, String, Option<usize>)| {
                Ok(this
                    .re
                    .replacen(&content, limit.unwrap_or_default(), replace)
                    .to_string())
            },
        );
    }
}
