use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct SankakuWebCore;

#[wasm_bindgen]
impl SankakuWebCore {
    #[wasm_bindgen(constructor)]
    pub fn new() -> SankakuWebCore {
        SankakuWebCore
    }

    pub fn get_identity(&self) -> String {
        "a1b2c3d4e5f60718293a4b5c6d7e8f90".to_string()
    }
}
