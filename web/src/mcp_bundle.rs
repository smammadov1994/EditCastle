use anyhow::Result;
use serde_json::{Value, json};
use std::{
    io::{Cursor, Write},
    path::Path,
};
use uuid::Uuid;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

// The file picker passes a path. The key itself stays in that private file.
const LAUNCHER: &str = "#!/bin/sh\n\
    set -eu\n\
    if [ \"$#\" -ne 3 ] || [ ! -x \"$1\" ] || [ ! -f \"$3\" ]; then\n\
        echo 'Choose the EditCastle bridge executable and agent key file in extension settings.' >&2\n\
        exit 1\n\
    fi\n\
    EDITROOM_MCP_KEY_FILE=\"$3\"\n\
    export EDITROOM_MCP_KEY_FILE\n\
    exec \"$1\" mcp-stdio \"$2\"\n";

pub fn zip(
    project: Uuid,
    title: &str,
    endpoint: &str,
    binary: Option<&Path>,
    key_file: Option<&Path>,
    tools: Vec<Value>,
) -> Result<Vec<u8>> {
    let manifest = json!({
        "manifest_version":"0.3",
        "name":crate::mcp_setup::server_name(project),
        "display_name":format!("EditCastle Studio · {title}"),
        "version":env!("CARGO_PKG_VERSION"),
        "description":"Edit the connected EditCastle Studio project from Claude Desktop.",
        "author":{"name":"EditCastle"},
        "compatibility":{"platforms":["darwin"]},
        "server":{
            "type":"binary",
            "entry_point":"server/editroom-mcp",
            "mcp_config":{
                "command":"/bin/sh",
                "args":[
                    "${__dirname}/server/editroom-mcp",
                    "${user_config.bridge_path}",
                    endpoint,
                    "${user_config.agent_key_file}"
                ]
            }
        },
        "user_config":{
            "bridge_path":{
                "type":"file","title":"EditCastle bridge executable",
                "description":"Choose the editcastle-web executable installed on this Mac.",
                "required":true
            },
            "agent_key_file":{
                "type":"file","title":"EditCastle agent key file",
                "description":"Choose the private file containing the key from Attached agents in Studio.",
                "required":true
            }
        },
        "tools":tools.iter().filter_map(|tool| {
            Some(json!({"name":tool["name"].as_str()?,"description":tool["description"].as_str()?}))
        }).collect::<Vec<_>>()
    });
    let mut manifest = manifest;
    for (name, file) in [("bridge_path", binary), ("agent_key_file", key_file)] {
        if let Some(path) = file.and_then(Path::to_str) {
            manifest["user_config"][name]["default"] = json!(path);
        }
    }

    let mut bundle = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    bundle.start_file("manifest.json", options)?;
    serde_json::to_writer_pretty(&mut bundle, &manifest)?;
    bundle.start_file("server/editroom-mcp", options.unix_permissions(0o755))?;
    bundle.write_all(LAUNCHER.as_bytes())?;
    Ok(bundle.finish()?.into_inner())
}
