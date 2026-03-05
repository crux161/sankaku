use wasm_bindgen::prelude::*;

macro_rules! println {
    ($($arg:tt)*) => {{
        web_sys::console::log_1(&JsValue::from_str(&format!($($arg)*)));
    }};
}

#[wasm_bindgen]
pub struct SankakuWebCore;

#[wasm_bindgen]
impl SankakuWebCore {
    #[wasm_bindgen(constructor)]
    pub fn new() -> SankakuWebCore {
        println!("SankakuWebCore initialized");
        SankakuWebCore
    }

    pub fn get_identity(&self) -> String {
        "a1b2c3d4e5f60718293a4b5c6d7e8f90".to_string()
    }
}
