//! Reading `pw-dump` output for per-app capture on PipeWire. Kept free
//! of the pipewire crate so it builds and tests on any OS.

use std::collections::HashSet;

use serde_json::Value;

use crate::AppInfo;

fn objects(dump: &Value) -> impl Iterator<Item = &Value> {
    dump.as_array().into_iter().flatten()
}

fn is_type(obj: &Value, ty: &str) -> bool {
    obj["type"].as_str() == Some(ty)
}

fn id(obj: &Value) -> Option<u32> {
    obj["id"].as_u64().map(|i| i as u32)
}

/// pw-dump prints some ids as numbers and some as strings.
fn num(v: &Value) -> Option<u32> {
    v.as_u64()
        .map(|n| n as u32)
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// App output streams (`Stream/Output/Audio` nodes).
pub fn apps(dump: &Value) -> Vec<AppInfo> {
    objects(dump)
        .filter(|o| is_type(o, "PipeWire:Interface:Node"))
        .filter_map(|o| {
            let props = &o["info"]["props"];
            if props["media.class"].as_str() != Some("Stream/Output/Audio") {
                return None;
            }
            let app_name = props["application.name"].as_str().unwrap_or_default();
            let name = props["application.process.binary"]
                .as_str()
                .or(props["node.name"].as_str())
                .unwrap_or(app_name);
            Some(AppInfo {
                pid: num(&props["application.process.id"]).unwrap_or(0),
                name: name.to_string(),
                id: app_name.to_string(),
                playing: o["info"]["state"].as_str() == Some("running"),
                object: id(o)?,
            })
        })
        .collect()
}

/// `(output port, input port)` pairs still to link so every audio output
/// port of the apps matching `want` feeds every input port of `node_name`.
pub fn missing_links(dump: &Value, want: &str, node_name: &str) -> Vec<(u32, u32)> {
    let sources: HashSet<u32> = apps(dump)
        .into_iter()
        .filter(|a| a.matches(want))
        .map(|a| a.object)
        .collect();
    let Some(ours) = objects(dump)
        .filter(|o| is_type(o, "PipeWire:Interface:Node"))
        .find(|o| o["info"]["props"]["node.name"].as_str() == Some(node_name))
        .and_then(id)
    else {
        return Vec::new();
    };
    let (mut outs, mut ins) = (Vec::new(), Vec::new());
    for port in objects(dump).filter(|o| is_type(o, "PipeWire:Interface:Port")) {
        let props = &port["info"]["props"];
        let (Some(pid), Some(node)) = (id(port), num(&props["node.id"])) else {
            continue;
        };
        let dir = port["info"]["direction"]
            .as_str()
            .or(props["port.direction"].as_str());
        let monitor = props["port.monitor"].as_bool() == Some(true)
            || props["port.monitor"].as_str() == Some("true");
        match dir {
            Some("output") | Some("out") if sources.contains(&node) && !monitor => outs.push(pid),
            Some("input") | Some("in") if node == ours => ins.push(pid),
            _ => {}
        }
    }
    let linked: HashSet<(u32, u32)> = objects(dump)
        .filter(|o| is_type(o, "PipeWire:Interface:Link"))
        .filter_map(|o| {
            let info = &o["info"];
            Some((num(&info["output-port-id"])?, num(&info["input-port-id"])?))
        })
        .collect();
    outs.iter()
        .flat_map(|&o| ins.iter().map(move |&i| (o, i)))
        .filter(|pair| !linked.contains(pair))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dump() -> Value {
        json!([
            {"id": 40, "type": "PipeWire:Interface:Node", "info": {"state": "running", "props": {
                "media.class": "Stream/Output/Audio", "application.name": "Firefox",
                "application.process.binary": "firefox", "application.process.id": "1234"}}},
            {"id": 41, "type": "PipeWire:Interface:Port", "info": {"direction": "output", "props": {"node.id": 40}}},
            {"id": 42, "type": "PipeWire:Interface:Port", "info": {"direction": "output", "props": {"node.id": 40}}},
            {"id": 50, "type": "PipeWire:Interface:Node", "info": {"state": "idle", "props": {
                "media.class": "Stream/Output/Audio", "application.name": "mpv",
                "application.process.binary": "mpv", "application.process.id": 99}}},
            {"id": 51, "type": "PipeWire:Interface:Port", "info": {"direction": "output", "props": {"node.id": 50}}},
            {"id": 60, "type": "PipeWire:Interface:Node", "info": {"props": {
                "media.class": "Stream/Input/Audio", "node.name": "vox_scribe-7"}}},
            {"id": 61, "type": "PipeWire:Interface:Port", "info": {"direction": "input", "props": {"node.id": 60}}},
            {"id": 70, "type": "PipeWire:Interface:Link", "info": {"output-port-id": 41, "input-port-id": 61}}
        ])
    }

    #[test]
    fn lists_app_streams() {
        let apps = apps(&dump());
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].object, 40);
        assert_eq!(apps[0].pid, 1234);
        assert_eq!(apps[0].name, "firefox");
        assert!(apps[0].playing);
        assert_eq!(apps[1].pid, 99);
        assert!(!apps[1].playing);
    }

    #[test]
    fn links_only_whats_missing() {
        assert_eq!(
            missing_links(&dump(), "FIREFOX", "vox_scribe-7"),
            vec![(42, 61)]
        );
        assert_eq!(missing_links(&dump(), "99", "vox_scribe-7"), vec![(51, 61)]);
        assert!(missing_links(&dump(), "chrome", "vox_scribe-7").is_empty());
        assert!(missing_links(&dump(), "mpv", "nobody").is_empty());
    }
}
