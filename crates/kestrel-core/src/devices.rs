// kestrel-rs :: devices — device emulation presets (ported from src/cdp/devices.js).
use serde_json::{Value, json};

pub struct Device {
    pub name: &'static str,
    pub width: u32,
    pub height: u32,
    pub dsf: f64,
    pub mobile: bool,
    pub ua: &'static str,
    pub platform: &'static str,
}

const UA_WIN: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36";
const UA_MAC: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36";
const UA_IPHONE: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1";
const UA_IPAD: &str = "Mozilla/5.0 (iPad; CPU OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1";
const UA_ANDROID: &str = "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Mobile Safari/537.36";
const UA_BOT: &str = "Mozilla/5.0 (compatible; VeloxBot/1.0; +https://kestrel.dev/bot)";

pub const DEVICES: &[Device] = &[
    Device {
        name: "desktop",
        width: 1366,
        height: 768,
        dsf: 1.0,
        mobile: false,
        ua: UA_WIN,
        platform: "Windows",
    },
    Device {
        name: "desktop_hd",
        width: 1920,
        height: 1080,
        dsf: 1.0,
        mobile: false,
        ua: UA_WIN,
        platform: "Windows",
    },
    Device {
        name: "mac",
        width: 1512,
        height: 982,
        dsf: 2.0,
        mobile: false,
        ua: UA_MAC,
        platform: "macOS",
    },
    Device {
        name: "iphone_13",
        width: 390,
        height: 844,
        dsf: 3.0,
        mobile: true,
        ua: UA_IPHONE,
        platform: "iPhone",
    },
    Device {
        name: "iphone_15",
        width: 393,
        height: 852,
        dsf: 3.0,
        mobile: true,
        ua: UA_IPHONE,
        platform: "iPhone",
    },
    Device {
        name: "iphone_se",
        width: 375,
        height: 667,
        dsf: 2.0,
        mobile: true,
        ua: UA_IPHONE,
        platform: "iPhone",
    },
    Device {
        name: "ipad",
        width: 820,
        height: 1180,
        dsf: 2.0,
        mobile: true,
        ua: UA_IPAD,
        platform: "iPad",
    },
    Device {
        name: "pixel_8",
        width: 412,
        height: 915,
        dsf: 2.625,
        mobile: true,
        ua: UA_ANDROID,
        platform: "Linux armv8l",
    },
    Device {
        name: "galaxy_s24",
        width: 384,
        height: 854,
        dsf: 3.0,
        mobile: true,
        ua: UA_ANDROID,
        platform: "Linux armv8l",
    },
    Device {
        name: "bot",
        width: 1280,
        height: 720,
        dsf: 1.0,
        mobile: false,
        ua: UA_BOT,
        platform: "Linux x86_64",
    },
];

pub fn get(name: &str) -> Option<&'static Device> {
    DEVICES.iter().find(|d| d.name == name)
}

/// CDP payload for Emulation.setDeviceMetricsOverride + UA override.
pub fn metrics(d: &Device) -> Value {
    json!({
        "width": d.width, "height": d.height, "deviceScaleFactor": d.dsf, "mobile": d.mobile,
    })
}
