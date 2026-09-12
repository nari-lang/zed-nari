use std::collections::HashMap;

use zed_extension_api::{
    self as zed, DebugAdapterBinary, DebugConfig, DebugRequest, DebugScenario, DebugTaskDefinition,
    LanguageServerId, Os, Result, StartDebuggingRequestArguments,
    StartDebuggingRequestArgumentsRequest,
    process::Command,
    serde_json::{Value, json}, 
    settings::LspSettings,
};

const LSP_BINARY: &str = "nari-lsp";
const ADAPTER_NAME: &str = "nari";
const INTERPRETER_BINARY: &str = "nari";

struct Nari {
    lsp_paths: HashMap<u64, String>,
    interpreter_paths: HashMap<u64, String>,
}

fn is_executable(path: &str) -> Result<bool> {
    let mut command = match current_os() {
        Os::Windows => Command::new("powershell")
            .arg("-NoProfile")
            .arg("-Command")
            .arg("if (Test-Path -LiteralPath $args[0] -PathType Leaf) { exit 0 } else { exit 1 }")
            .arg(path),
        _ => Command::new("sh").arg("-c").arg("test -x \"$0\"").arg(path),
    };

    command
        .output()
        .map(|output| output.status == Some(0))
        .map_err(|err| format!("could not check whether `{path}` is executable: {err}"))
}

fn current_os() -> Os {
    let (os, _arch) = zed::current_platform();
    os
}

fn exe_suffix() -> &'static str {
    match current_os() {
        Os::Windows => ".exe",
        _ => "",
    }
}

fn find_binary(
    cache: &mut HashMap<u64, String>,
    worktree: &zed::Worktree,
    name: &str,
    configured: Option<String>,
    setting: &str,
) -> Result<String> {
    if let Some(path) = configured {
        let path = path.trim().to_string();
        if !path.is_empty() {
            if is_executable(&path)? {
                return Ok(path);
            }
            return Err(format!(
                "`{setting}` is set to `{path}`, but that is not an executable file."
            ));
        }
    }

    if let Some(cached) = cache.get(&worktree.id()) {
        return Ok(cached.clone());
    }

    let root = worktree.root_path();
    let suffix = exe_suffix();
    let candidates = [
        format!("{root}/build/debug/{name}{suffix}"),
        format!("{root}/build/release/{name}{suffix}"),
    ];

    for candidate in &candidates {
        if is_executable(candidate)? {
            cache.insert(worktree.id(), candidate.clone());
            return Ok(candidate.clone());
        }
    }

    if let Some(found) = worktree.which(name) {
        cache.insert(worktree.id(), found.clone());
        return Ok(found);
    }

    Err(format!(
        "could not find the `{name}` binary. Build Nari (./build.sh), put `{name}` on \
         $PATH, or set `{setting}` in your Zed settings.\nWorktree root: {root}\nTried:\n  {}\n  \
         `{name}` on $PATH",
        candidates.join("\n  "),
    ))
}

#[cfg_attr(test, derive(Debug))]
struct LaunchConfig {
    request_args: String,
    cwd: Option<String>,
    envs: Vec<(String, String)>,
}

fn parse_launch_config(config: &str) -> Result<LaunchConfig> {
    let mut value: Value = if config.trim().is_empty() {
        json!({})
    } else {
        zed::serde_json::from_str(config).map_err(|err| format!("invalid debug config: {err}"))?
    };

    let object = value
        .as_object_mut()
        .ok_or_else(|| "debug config must be a JSON object".to_string())?;
    
    object.remove("request");

    let cwd = object
        .remove("cwd")
        .and_then(|cwd| cwd.as_str().map(str::to_string))
        .filter(|cwd| !cwd.trim().is_empty());

    let envs = object
        .remove("env")
        .and_then(|env| match env {
            Value::Object(env) => Some(env),
            _ => None,
        })
        .map(|env| {
            env.into_iter()
                .map(|(key, value)| {
                    let value = match value {
                        Value::String(value) => value,
                        other => other.to_string(),
                    };
                    (key, value)
                })
                .collect()
        })
        .unwrap_or_default();

    let program = object.get("program").and_then(Value::as_str).unwrap_or("");
    if program.trim().is_empty() {
        return Err(
            "no `program` field in the launch configuration: set it to the .nari or \
             .naric file to debug (e.g. \"$ZED_FILE\")."
                .to_string(),
        );
    }

    object
        .entry("stopOnEntry")
        .or_insert_with(|| Value::Bool(true));
    object.entry("args").or_insert_with(|| json!([]));

    Ok(LaunchConfig {
        request_args: value.to_string(),
        cwd,
        envs,
    })
}

fn unsupported_attach() -> String {
    "the Nari debug adapter does not support attaching to a running process; \
     use a `launch` request instead."
        .to_string()
}

impl zed::Extension for Nari {
    fn new() -> Self {
        Self {
            lsp_paths: HashMap::new(),
            interpreter_paths: HashMap::new(),
        }
    }
    
    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let binary = LspSettings::for_worktree(language_server_id.as_ref(), worktree)
            .ok()
            .and_then(|settings| settings.binary);

        let command = find_binary(
            &mut self.lsp_paths,
            worktree,
            LSP_BINARY,
            binary.as_ref().and_then(|binary| binary.path.clone()),
            &format!("lsp.{}.binary.path", language_server_id.as_ref()),
        )?;

        Ok(zed::Command {
            command,
            args: binary
                .as_ref()
                .and_then(|binary| binary.arguments.clone())
                .unwrap_or_default(),
            env: binary
                .and_then(|binary| binary.env)
                .map(|env| env.into_iter().collect())
                .unwrap_or_default(),
        })
    }

    fn language_server_initialization_options(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<Value>> {
        Ok(
            LspSettings::for_worktree(language_server_id.as_ref(), worktree)
                .ok()
                .and_then(|settings| settings.initialization_options),
        )
    }

    fn language_server_workspace_configuration(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<Value>> {
        Ok(
            LspSettings::for_worktree(language_server_id.as_ref(), worktree)
                .ok()
                .and_then(|settings| settings.settings),
        )
    }

    fn get_dap_binary(
        &mut self,
        adapter_name: String,
        config: DebugTaskDefinition,
        user_provided_debug_adapter_path: Option<String>,
        worktree: &zed::Worktree,
    ) -> Result<DebugAdapterBinary> {
        if adapter_name != ADAPTER_NAME {
            return Err(format!("unknown debug adapter `{adapter_name}`"));
        }

        let command = find_binary(
            &mut self.interpreter_paths,
            worktree,
            INTERPRETER_BINARY,
            user_provided_debug_adapter_path,
            &format!("debug_adapters.{ADAPTER_NAME}.binary"),
        )?;

        let launch = parse_launch_config(&config.config)?;

        Ok(DebugAdapterBinary {
            command: Some(command),
            arguments: vec!["--dap".to_string()],
            envs: launch.envs,
            cwd: Some(launch.cwd.unwrap_or_else(|| worktree.root_path())),
            connection: None,
            request_args: StartDebuggingRequestArguments {
                configuration: launch.request_args,
                request: StartDebuggingRequestArgumentsRequest::Launch,
            },
        })
    }

    fn dap_request_kind(
        &mut self,
        adapter_name: String,
        config: Value,
    ) -> Result<StartDebuggingRequestArgumentsRequest> {
        if adapter_name != ADAPTER_NAME {
            return Err(format!("unknown debug adapter `{adapter_name}`"));
        }

        if config.get("request").and_then(Value::as_str) == Some("attach") {
            return Err(unsupported_attach());
        }

        Ok(StartDebuggingRequestArgumentsRequest::Launch)
    }

    fn dap_config_to_scenario(&mut self, config: DebugConfig) -> Result<DebugScenario> {
        if config.adapter != ADAPTER_NAME {
            return Err(format!("unknown debug adapter `{}`", config.adapter));
        }

        let launch = match config.request {
            DebugRequest::Launch(launch) => launch,
            DebugRequest::Attach(_) => return Err(unsupported_attach()),
        };

        let mut scenario_config = json!({
            "program": launch.program,
            "args": launch.args,
            "stopOnEntry": config.stop_on_entry.unwrap_or(true),
        });

        if let Some(cwd) = launch.cwd {
            scenario_config["cwd"] = Value::String(cwd);
        }
        if !launch.envs.is_empty() {
            scenario_config["env"] = launch
                .envs
                .into_iter()
                .map(|(key, value)| (key, Value::String(value)))
                .collect::<zed::serde_json::Map<_, _>>()
                .into();
        }

        Ok(DebugScenario {
            label: config.label,
            adapter: config.adapter,
            build: None,
            config: scenario_config.to_string(),
            tcp_connection: None,
        })
    }
}

zed::register_extension!(Nari);
