//! Reading `pw-dump` output: per-app capture, and the channel layout a
//! source runs in. Kept free of the pipewire crate so it builds and
//! tests on any OS.

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

/// The channel layout a node runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub channels: usize,
    /// What each channel is — `FL`, `MONO`, `AUX3`, … — as the node
    /// names them. Empty when it doesn't say; shorter than `channels`
    /// never happens in practice, and the caller fills the rest in.
    pub positions: Vec<String>,
}

/// The layout of the node named `node`: its `audio.channels` and
/// `audio.position` props, falling back to the channels and positions of
/// its first announced format. `None` when the dump hasn't got the node,
/// or it says nothing about its channels.
pub fn layout(dump: &Value, node: &str) -> Option<Layout> {
    layout_of(
        objects(dump)
            .filter(|o| is_type(o, "PipeWire:Interface:Node"))
            .find(|o| o["info"]["props"]["node.name"].as_str() == Some(node))?,
    )
}

/// [`layout`] for a node object already in hand.
pub fn layout_of(node: &Value) -> Option<Layout> {
    let props = &node["info"]["props"];
    let format = &node["info"]["params"]["EnumFormat"][0];
    let channels = num(&props["audio.channels"])
        .or_else(|| num(&format["channels"]))
        .filter(|&n| n > 0)? as usize;
    // The prop is a printed list (`[ FL, FR ]`, or `[MONO]` from
    // module-loopback); the format has it as a real array.
    let positions = match props["audio.position"].as_str() {
        Some(s) => s
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .split(',')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect(),
        None => format["position"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| p.as_str().map(str::to_string))
            .collect(),
    };
    Some(Layout {
        channels,
        positions,
    })
}

/// The `node.name` of the source or sink that `want` names: that name
/// exactly, else a node whose name or description contains it — the
/// same loose match the device list offers.
pub fn find_source(dump: &Value, want: &str) -> Option<String> {
    let nodes: Vec<&Value> = objects(dump)
        .filter(|o| is_type(o, "PipeWire:Interface:Node"))
        .filter(|o| {
            matches!(
                o["info"]["props"]["media.class"].as_str(),
                Some("Audio/Source" | "Audio/Source/Virtual" | "Audio/Sink")
            )
        })
        .collect();
    let name = |o: &Value| o["info"]["props"]["node.name"].as_str().map(str::to_string);
    if let Some(exact) = nodes
        .iter()
        .find(|o| o["info"]["props"]["node.name"].as_str() == Some(want))
    {
        return name(exact);
    }
    let needle = want.to_lowercase();
    let has = |v: &Value| {
        v.as_str()
            .is_some_and(|s| s.to_lowercase().contains(&needle))
    };
    nodes
        .iter()
        .find(|o| {
            has(&o["info"]["props"]["node.name"]) || has(&o["info"]["props"]["node.description"])
        })
        .and_then(|o| name(o))
}

/// The node name of the default audio source, from the `default`
/// metadata the session manager keeps. That is what a stream with no
/// `target.object` autoconnects to.
pub fn default_source(dump: &Value) -> Option<&str> {
    objects(dump)
        .filter(|o| is_type(o, "PipeWire:Interface:Metadata"))
        .filter(|o| o["props"]["metadata.name"].as_str() == Some("default"))
        .flat_map(|o| o["metadata"].as_array().into_iter().flatten())
        .find(|e| e["key"].as_str() == Some("default.audio.source"))
        .and_then(|e| e["value"]["name"].as_str())
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

    /// Nodes as pw-dump gives them: a stereo source whose props say the
    /// layout, one that only says it in its format, and a mono one.
    fn sources() -> Value {
        json!([
            {"id": 80, "type": "PipeWire:Interface:Node", "info": {"props": {
                "media.class": "Audio/Source", "node.name": "rx",
                "audio.channels": 2, "audio.position": "[ FL, FR ]"}}},
            {"id": 81, "type": "PipeWire:Interface:Node", "info": {
                "props": {"media.class": "Audio/Source", "node.name": "quiet"},
                "params": {"EnumFormat": [{"channels": 4,
                    "position": ["FL", "FR", "RL", "RR"]}]}}},
            {"id": 82, "type": "PipeWire:Interface:Node", "info": {"props": {
                "media.class": "Audio/Source", "node.name": "headset",
                "audio.channels": "1", "audio.position": "[MONO]"}}},
            {"id": 83, "type": "PipeWire:Interface:Node", "info": {"props": {
                "media.class": "Audio/Source", "node.name": "mystery"}}},
            {"id": 90, "type": "PipeWire:Interface:Metadata",
             "props": {"metadata.name": "default"},
             "metadata": [
                {"key": "default.audio.sink", "value": {"name": "speakers"}},
                {"key": "default.audio.source", "value": {"name": "headset"}}]}
        ])
    }

    #[test]
    fn reads_the_channel_layout_off_a_node() {
        let d = sources();
        assert_eq!(
            layout(&d, "rx"),
            Some(Layout {
                channels: 2,
                positions: vec!["FL".into(), "FR".into()]
            })
        );
        // No props: the announced format says it instead.
        assert_eq!(layout(&d, "quiet").unwrap().channels, 4);
        assert_eq!(layout(&d, "quiet").unwrap().positions.len(), 4);
        // A string count, and module-loopback's unspaced position list.
        assert_eq!(
            layout(&d, "headset"),
            Some(Layout {
                channels: 1,
                positions: vec!["MONO".into()]
            })
        );
        // Says nothing, or isn't there at all.
        assert_eq!(layout(&d, "mystery"), None);
        assert_eq!(layout(&d, "nobody"), None);
    }

    #[test]
    fn finds_the_default_source() {
        assert_eq!(default_source(&sources()), Some("headset"));
        assert_eq!(default_source(&dump()), None);
    }

    #[test]
    fn finds_a_source_by_name_or_substring() {
        let d = sources();
        assert_eq!(find_source(&d, "rx").as_deref(), Some("rx"));
        assert_eq!(find_source(&d, "HEADSET").as_deref(), Some("headset"));
        assert_eq!(find_source(&d, "nobody"), None);
        // An app stream is not a device.
        assert_eq!(find_source(&dump(), "vox_scribe-7"), None);
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
