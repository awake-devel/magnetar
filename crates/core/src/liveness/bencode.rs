//! Lenient bencode decoder/encoder (accepts unsorted dict keys).

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Int(i64),
    Bytes(Vec<u8>),
    List(Vec<Value>),
    Dict(Vec<(Vec<u8>, Value)>),
}

pub fn decode(input: &[u8]) -> Result<Value, String> {
    let _ = input;
    todo!("bencode::decode")
}

pub fn encode(v: &Value) -> Vec<u8> {
    let _ = v;
    todo!("bencode::encode")
}
