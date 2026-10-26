//! KRPC messages (BEP 5) with the BEP 42 `ip` field, BEP 43 `ro` flag and BEP 51
//! `sample_infohashes`.
//!
//! Every byte comes from the network and is attacker-controlled (docs/DHT.md §3.8): decoding never
//! panics, and a message with a malformed field is dropped as a whole. Ids and infohashes must be
//! exactly 20 bytes, `t` 1–8 bytes, compact `nodes` a multiple of 26 bytes, `values` entries exactly
//! 6 bytes, `samples` a multiple of 20 bytes (BEP 51 "N × 20 bytes"), ports non-zero. At most
//! [`MAX_SAMPLES`] samples, [`MAX_NODES`] nodes and [`MAX_VALUES`] values are kept per message.

use std::net::{Ipv4Addr, SocketAddrV4};

use crate::liveness::bencode::{self, Value};

pub type Id = [u8; 20];

/// Largest datagram accepted; a longer one is dropped (docs/DHT.md §3.8).
pub const MAX_DATAGRAM: usize = 2048;
pub const MAX_SAMPLES: usize = 20;
pub const MAX_NODES: usize = 16;
pub const MAX_VALUES: usize = 50;
/// Tokens longer than this are refused (ours are 8 bytes; libtorrent's 4).
pub const MAX_TOKEN: usize = 64;
const MAX_TID: usize = 8;
const MAX_ERROR_TEXT: usize = 128;

/// BEP 5 error codes.
pub const ERR_GENERIC: i64 = 201;
pub const ERR_PROTOCOL: i64 = 203;
pub const ERR_METHOD_UNKNOWN: i64 = 204;

/// A node's compact contact (BEP 5 "Compact node info", 26 bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CompactNode {
    pub id: Id,
    pub addr: SocketAddrV4,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Query {
    Ping,
    FindNode {
        target: Id,
    },
    GetPeers {
        info_hash: Id,
    },
    AnnouncePeer {
        info_hash: Id,
        port: u16,
        implied_port: bool,
        token: Vec<u8>,
    },
    SampleInfohashes {
        target: Id,
    },
    /// Any other method (answered with error 204). Only the name length is bounded by the packet.
    Unknown(Vec<u8>),
}

impl Query {
    pub fn method(&self) -> &[u8] {
        match self {
            Query::Ping => b"ping",
            Query::FindNode { .. } => b"find_node",
            Query::GetPeers { .. } => b"get_peers",
            Query::AnnouncePeer { .. } => b"announce_peer",
            Query::SampleInfohashes { .. } => b"sample_infohashes",
            Query::Unknown(m) => m,
        }
    }
}

/// Response fields; which ones are present depends on the query (responses are not
/// self-describing in BEP 5).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Response {
    pub id: Id,
    pub nodes: Vec<CompactNode>,
    pub values: Vec<SocketAddrV4>,
    pub token: Option<Vec<u8>>,
    /// `None` when the field is absent: BEP 51 supporters "should always include the samples
    /// field ... even when it is zero-length", so absence marks a non-supporter.
    pub samples: Option<Vec<Id>>,
    pub interval: Option<i64>,
    pub num: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    Query { id: Id, query: Query, ro: bool },
    Response(Response),
    Error { code: i64, message: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// Transaction id, 1–8 bytes.
    pub tid: Vec<u8>,
    pub body: Body,
    /// BEP 42: the requester's address as the responder saw it (top level, responses).
    pub ip: Option<SocketAddrV4>,
    /// BEP 5 `v`: client id + version (first 4 bytes kept).
    pub version: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    TooLong,
    Bencode,
    NotADict,
    BadTid,
    BadType,
    MissingField(&'static str),
    BadField(&'static str),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::TooLong => write!(f, "datagram too long"),
            DecodeError::Bencode => write!(f, "bad bencode"),
            DecodeError::NotADict => write!(f, "not a dict"),
            DecodeError::BadTid => write!(f, "bad transaction id"),
            DecodeError::BadType => write!(f, "bad message type"),
            DecodeError::MissingField(k) => write!(f, "missing {k}"),
            DecodeError::BadField(k) => write!(f, "bad {k}"),
        }
    }
}

impl std::error::Error for DecodeError {}

fn id20(v: Option<&Value>, name: &'static str) -> Result<Id, DecodeError> {
    let b = v
        .ok_or(DecodeError::MissingField(name))?
        .as_bytes()
        .ok_or(DecodeError::BadField(name))?;
    <[u8; 20]>::try_from(b).map_err(|_| DecodeError::BadField(name))
}

/// Compact peer info (6 bytes).
pub fn parse_compact_addr(b: &[u8]) -> Option<SocketAddrV4> {
    let b: &[u8; 6] = b.try_into().ok()?;
    let port = u16::from_be_bytes([b[4], b[5]]);
    (port != 0).then(|| SocketAddrV4::new(Ipv4Addr::new(b[0], b[1], b[2], b[3]), port))
}

pub fn compact_addr(a: &SocketAddrV4) -> [u8; 6] {
    let o = a.ip().octets();
    let p = a.port().to_be_bytes();
    [o[0], o[1], o[2], o[3], p[0], p[1]]
}

/// Compact node list: a multiple of 26 bytes, else an error. Entries with port 0 are skipped;
/// at most [`MAX_NODES`] are kept.
pub fn parse_nodes(b: &[u8]) -> Result<Vec<CompactNode>, DecodeError> {
    if !b.len().is_multiple_of(26) {
        return Err(DecodeError::BadField("nodes"));
    }
    Ok(b.chunks_exact(26)
        .filter_map(|c| {
            let id: Id = c.get(..20)?.try_into().ok()?;
            let addr = parse_compact_addr(c.get(20..26)?)?;
            Some(CompactNode { id, addr })
        })
        .take(MAX_NODES)
        .collect())
}

pub fn encode_nodes(nodes: &[CompactNode]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nodes.len() * 26);
    for n in nodes {
        out.extend_from_slice(&n.id);
        out.extend_from_slice(&compact_addr(&n.addr));
    }
    out
}

/// Decode one datagram.
pub fn decode(buf: &[u8]) -> Result<Message, DecodeError> {
    if buf.len() > MAX_DATAGRAM {
        return Err(DecodeError::TooLong);
    }
    let v = bencode::decode(buf).map_err(|_| DecodeError::Bencode)?;
    if v.as_dict().is_none() {
        return Err(DecodeError::NotADict);
    }
    let tid = v
        .get(b"t")
        .and_then(Value::as_bytes)
        .filter(|t| (1..=MAX_TID).contains(&t.len()))
        .ok_or(DecodeError::BadTid)?
        .to_vec();
    let ip = match v.get(b"ip") {
        None => None,
        // Only IPv4 is spoken (BEP 32 is out of scope); an 18-byte IPv6 `ip` is ignored.
        Some(x) => x.as_bytes().and_then(parse_compact_addr),
    };
    let version = v
        .get(b"v")
        .and_then(Value::as_bytes)
        .map(|b| b.iter().take(4).copied().collect());
    let body = match v.get(b"y").and_then(Value::as_bytes) {
        Some(b"q") => decode_query(&v)?,
        Some(b"r") => Body::Response(decode_response(
            v.get(b"r").ok_or(DecodeError::MissingField("r"))?,
        )?),
        Some(b"e") => decode_error(v.get(b"e").ok_or(DecodeError::MissingField("e"))?)?,
        _ => return Err(DecodeError::BadType),
    };
    Ok(Message {
        tid,
        body,
        ip,
        version,
    })
}

fn decode_query(v: &Value) -> Result<Body, DecodeError> {
    let method = v
        .get(b"q")
        .and_then(Value::as_bytes)
        .ok_or(DecodeError::MissingField("q"))?;
    let a = v.get(b"a").ok_or(DecodeError::MissingField("a"))?;
    if a.as_dict().is_none() {
        return Err(DecodeError::BadField("a"));
    }
    let id = id20(a.get(b"id"), "id")?;
    let ro = v.get(b"ro").and_then(Value::as_int) == Some(1);
    let query = match method {
        b"ping" => Query::Ping,
        b"find_node" => Query::FindNode {
            target: id20(a.get(b"target"), "target")?,
        },
        b"get_peers" => Query::GetPeers {
            info_hash: id20(a.get(b"info_hash"), "info_hash")?,
        },
        b"sample_infohashes" => Query::SampleInfohashes {
            target: id20(a.get(b"target"), "target")?,
        },
        b"announce_peer" => {
            let info_hash = id20(a.get(b"info_hash"), "info_hash")?;
            let implied_port = a.get(b"implied_port").and_then(Value::as_int).unwrap_or(0) != 0;
            let port = match a.get(b"port").and_then(Value::as_int) {
                Some(p) => u16::try_from(p).map_err(|_| DecodeError::BadField("port"))?,
                None if implied_port => 0,
                None => return Err(DecodeError::MissingField("port")),
            };
            // BEP 5: with implied_port the source port is used and `port` is ignored.
            if port == 0 && !implied_port {
                return Err(DecodeError::BadField("port"));
            }
            let token = a
                .get(b"token")
                .and_then(Value::as_bytes)
                .filter(|t| !t.is_empty() && t.len() <= MAX_TOKEN)
                .ok_or(DecodeError::BadField("token"))?
                .to_vec();
            Query::AnnouncePeer {
                info_hash,
                port,
                implied_port,
                token,
            }
        }
        other => Query::Unknown(other.iter().take(32).copied().collect()),
    };
    Ok(Body::Query { id, query, ro })
}

fn decode_response(r: &Value) -> Result<Response, DecodeError> {
    if r.as_dict().is_none() {
        return Err(DecodeError::BadField("r"));
    }
    let id = id20(r.get(b"id"), "id")?;
    let nodes = match r.get(b"nodes") {
        None => Vec::new(),
        Some(n) => parse_nodes(n.as_bytes().ok_or(DecodeError::BadField("nodes"))?)?,
    };
    let values = match r.get(b"values") {
        None => Vec::new(),
        Some(list) => {
            let list = list.as_list().ok_or(DecodeError::BadField("values"))?;
            let mut out = Vec::new();
            for p in list {
                let p = p.as_bytes().ok_or(DecodeError::BadField("values"))?;
                if p.len() != 6 {
                    return Err(DecodeError::BadField("values"));
                }
                if out.len() < MAX_VALUES
                    && let Some(a) = parse_compact_addr(p)
                {
                    out.push(a);
                }
            }
            out
        }
    };
    let token = match r.get(b"token") {
        None => None,
        Some(t) => Some(
            t.as_bytes()
                .filter(|t| t.len() <= MAX_TOKEN)
                .ok_or(DecodeError::BadField("token"))?
                .to_vec(),
        ),
    };
    let samples = match r.get(b"samples") {
        None => None,
        Some(s) => {
            let s = s.as_bytes().ok_or(DecodeError::BadField("samples"))?;
            if !s.len().is_multiple_of(20) {
                return Err(DecodeError::BadField("samples"));
            }
            Some(
                s.chunks_exact(20)
                    .take(MAX_SAMPLES)
                    .filter_map(|c| <[u8; 20]>::try_from(c).ok())
                    .collect(),
            )
        }
    };
    let int = |k: &'static str| -> Result<Option<i64>, DecodeError> {
        match r.get(k.as_bytes()) {
            None => Ok(None),
            Some(x) => x.as_int().map(Some).ok_or(DecodeError::BadField(k)),
        }
    };
    Ok(Response {
        id,
        nodes,
        values,
        token,
        samples,
        interval: int("interval")?,
        num: int("num")?,
    })
}

fn decode_error(e: &Value) -> Result<Body, DecodeError> {
    let l = e.as_list().ok_or(DecodeError::BadField("e"))?;
    let code = l
        .first()
        .and_then(Value::as_int)
        .ok_or(DecodeError::BadField("e"))?;
    let message = l
        .get(1)
        .and_then(Value::as_bytes)
        .map(|b| {
            String::from_utf8_lossy(b)
                .chars()
                .take(MAX_ERROR_TEXT)
                .collect()
        })
        .unwrap_or_default();
    Ok(Body::Error { code, message })
}

fn bytes(b: &[u8]) -> Value {
    Value::Bytes(b.to_vec())
}

/// Encode a message (keys sorted, canonical bencode).
pub fn encode(m: &Message) -> Vec<u8> {
    let mut top: Vec<(Vec<u8>, Value)> = vec![(b"t".to_vec(), bytes(&m.tid))];
    match &m.body {
        Body::Query { id, query, ro } => {
            let mut a = vec![(b"id".to_vec(), bytes(id))];
            match query {
                Query::Ping | Query::Unknown(_) => {}
                Query::FindNode { target } | Query::SampleInfohashes { target } => {
                    a.push((b"target".to_vec(), bytes(target)))
                }
                Query::GetPeers { info_hash } => a.push((b"info_hash".to_vec(), bytes(info_hash))),
                Query::AnnouncePeer {
                    info_hash,
                    port,
                    implied_port,
                    token,
                } => {
                    a.push((
                        b"implied_port".to_vec(),
                        Value::Int(i64::from(*implied_port)),
                    ));
                    a.push((b"info_hash".to_vec(), bytes(info_hash)));
                    a.push((b"port".to_vec(), Value::Int(i64::from(*port))));
                    a.push((b"token".to_vec(), bytes(token)));
                }
            }
            top.push((b"y".to_vec(), bytes(b"q")));
            top.push((b"q".to_vec(), bytes(query.method())));
            top.push((b"a".to_vec(), Value::Dict(a)));
            if *ro {
                top.push((b"ro".to_vec(), Value::Int(1)));
            }
        }
        Body::Response(r) => {
            let mut d = vec![(b"id".to_vec(), bytes(&r.id))];
            if !r.nodes.is_empty() {
                d.push((b"nodes".to_vec(), bytes(&encode_nodes(&r.nodes))));
            }
            if !r.values.is_empty() {
                d.push((
                    b"values".to_vec(),
                    Value::List(r.values.iter().map(|a| bytes(&compact_addr(a))).collect()),
                ));
            }
            if let Some(t) = &r.token {
                d.push((b"token".to_vec(), bytes(t)));
            }
            if let Some(s) = &r.samples {
                d.push((b"samples".to_vec(), bytes(&s.concat())));
            }
            if let Some(i) = r.interval {
                d.push((b"interval".to_vec(), Value::Int(i)));
            }
            if let Some(n) = r.num {
                d.push((b"num".to_vec(), Value::Int(n)));
            }
            top.push((b"y".to_vec(), bytes(b"r")));
            top.push((b"r".to_vec(), Value::Dict(d)));
        }
        Body::Error { code, message } => {
            top.push((b"y".to_vec(), bytes(b"e")));
            top.push((
                b"e".to_vec(),
                Value::List(vec![Value::Int(*code), bytes(message.as_bytes())]),
            ));
        }
    }
    if let Some(ip) = &m.ip {
        top.push((b"ip".to_vec(), bytes(&compact_addr(ip))));
    }
    if let Some(v) = &m.version {
        top.push((b"v".to_vec(), bytes(v)));
    }
    bencode::encode(&Value::Dict(top))
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &Id = b"abcdefghij0123456789";
    const M: &Id = b"mnopqrstuvwxyz123456";

    fn q(id: &Id, query: Query) -> Message {
        Message {
            tid: b"aa".to_vec(),
            body: Body::Query {
                id: *id,
                query,
                ro: false,
            },
            ip: None,
            version: None,
        }
    }

    fn r(resp: Response) -> Message {
        Message {
            tid: b"aa".to_vec(),
            body: Body::Response(resp),
            ip: None,
            version: None,
        }
    }

    /// Each BEP 5 example packet decodes to the expected message and re-encodes byte for byte.
    #[test]
    fn bep5_examples_round_trip() {
        let cases: Vec<(&[u8], Message)> = vec![
            (
                b"d1:ad2:id20:abcdefghij0123456789e1:q4:ping1:t2:aa1:y1:qe",
                q(A, Query::Ping),
            ),
            (
                b"d1:rd2:id20:mnopqrstuvwxyz123456e1:t2:aa1:y1:re",
                r(Response {
                    id: *M,
                    ..Default::default()
                }),
            ),
            (
                b"d1:ad2:id20:abcdefghij01234567896:target20:mnopqrstuvwxyz123456e1:q9:find_node1:t2:aa1:y1:qe",
                q(A, Query::FindNode { target: *M }),
            ),
            (
                b"d1:ad2:id20:abcdefghij01234567899:info_hash20:mnopqrstuvwxyz123456e1:q9:get_peers1:t2:aa1:y1:qe",
                q(A, Query::GetPeers { info_hash: *M }),
            ),
            (
                b"d1:rd2:id20:abcdefghij01234567895:token8:aoeusnth6:valuesl6:axje.u6:idhtnmee1:t2:aa1:y1:re",
                r(Response {
                    id: *A,
                    token: Some(b"aoeusnth".to_vec()),
                    values: vec![
                        parse_compact_addr(b"axje.u").unwrap(),
                        parse_compact_addr(b"idhtnm").unwrap(),
                    ],
                    ..Default::default()
                }),
            ),
            (
                b"d1:ad2:id20:abcdefghij012345678912:implied_porti1e9:info_hash20:mnopqrstuvwxyz1234564:porti6881e5:token8:aoeusnthe1:q13:announce_peer1:t2:aa1:y1:qe",
                q(
                    A,
                    Query::AnnouncePeer {
                        info_hash: *M,
                        port: 6881,
                        implied_port: true,
                        token: b"aoeusnth".to_vec(),
                    },
                ),
            ),
            (
                b"d1:eli201e23:A Generic Error Ocurrede1:t2:aa1:y1:ee",
                Message {
                    tid: b"aa".to_vec(),
                    body: Body::Error {
                        code: 201,
                        message: "A Generic Error Ocurred".into(),
                    },
                    ip: None,
                    version: None,
                },
            ),
        ];
        for (wire, msg) in cases {
            let got =
                decode(wire).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(wire)));
            assert_eq!(got, msg, "{}", String::from_utf8_lossy(wire));
            assert_eq!(encode(&msg), wire, "{}", String::from_utf8_lossy(wire));
        }
    }

    /// BEP 5's find_node / get_peers "nodes" examples use the placeholder `9:def456...`, which is
    /// not a multiple of 26 bytes: such a message is dropped.
    #[test]
    fn bep5_placeholder_nodes_are_rejected() {
        let wire = b"d1:rd2:id20:0123456789abcdefghij5:nodes9:def456...e1:t2:aa1:y1:re";
        assert_eq!(decode(wire), Err(DecodeError::BadField("nodes")));
    }

    #[test]
    fn bep51_sample_infohashes_round_trip() {
        let msg = q(A, Query::SampleInfohashes { target: *M });
        let wire = encode(&msg);
        assert_eq!(
            wire,
            b"d1:ad2:id20:abcdefghij01234567896:target20:mnopqrstuvwxyz123456e1:q17:sample_infohashes1:t2:aa1:y1:qe"
        );
        assert_eq!(decode(&wire).unwrap(), msg);

        let node = CompactNode {
            id: [7; 20],
            addr: "1.2.3.4:6881".parse().unwrap(),
        };
        let resp = Response {
            id: *M,
            nodes: vec![node],
            samples: Some(vec![[1; 20], [2; 20]]),
            interval: Some(21600),
            num: Some(150),
            ..Default::default()
        };
        let back = decode(&encode(&r(resp.clone()))).unwrap();
        assert_eq!(back.body, Body::Response(resp));
        // Zero-length samples (BEP 51: "even when it is zero-length") is Some(empty).
        let empty =
            b"d1:rd2:id20:mnopqrstuvwxyz1234568:intervali300e3:numi0e7:samples0:e1:t2:aa1:y1:re";
        match decode(empty).unwrap().body {
            Body::Response(r) => {
                assert_eq!(r.samples, Some(vec![]));
                assert_eq!(r.interval, Some(300));
            }
            b => panic!("{b:?}"),
        }
        // No samples field: a non-supporter that answered like find_node.
        match decode(b"d1:rd2:id20:mnopqrstuvwxyz123456e1:t2:aa1:y1:re")
            .unwrap()
            .body
        {
            Body::Response(r) => assert_eq!(r.samples, None),
            b => panic!("{b:?}"),
        }
    }

    #[test]
    fn bep42_ip_and_bep43_ro() {
        let mut msg = q(A, Query::Ping);
        if let Body::Query { ro, .. } = &mut msg.body {
            *ro = true;
        }
        let wire = encode(&msg);
        // BEP 43: "This will appear in the request as '2:roi1e'".
        assert!(wire.windows(7).any(|w| w == b"2:roi1e"));
        assert_eq!(decode(&wire).unwrap(), msg);

        let mut resp = r(Response {
            id: *M,
            ..Default::default()
        });
        resp.ip = Some("124.31.75.21:6881".parse().unwrap());
        resp.version = Some(b"MG\x00\x01".to_vec());
        let wire = encode(&resp);
        assert_eq!(decode(&wire).unwrap(), resp);
    }

    #[test]
    fn caps_and_strict_lengths() {
        // 25 samples → only 20 kept; 21 nodes → 16.
        let resp = Response {
            id: *M,
            samples: Some(vec![[9; 20]; 25]),
            nodes: vec![
                CompactNode {
                    id: [3; 20],
                    addr: "5.6.7.8:1".parse().unwrap()
                };
                21
            ],
            values: vec!["9.9.9.9:9".parse().unwrap(); 60],
            ..Default::default()
        };
        match decode(&encode(&r(resp))).unwrap().body {
            Body::Response(r) => {
                assert_eq!(r.samples.unwrap().len(), MAX_SAMPLES);
                assert_eq!(r.nodes.len(), MAX_NODES);
                assert_eq!(r.values.len(), MAX_VALUES);
            }
            b => panic!("{b:?}"),
        }
        let bad: &[&[u8]] = &[
            // samples not a multiple of 20
            b"d1:rd2:id20:mnopqrstuvwxyz1234567:samples3:abce1:t2:aa1:y1:re",
            // id of 19 bytes
            b"d1:rd2:id19:mnopqrstuvwxyz12345e1:t2:aa1:y1:re",
            // empty tid, then a 9-byte tid
            b"d1:rd2:id20:mnopqrstuvwxyz123456e1:t0:1:y1:re",
            b"d1:rd2:id20:mnopqrstuvwxyz123456e1:t9:1234567891:y1:re",
            // a value of 18 bytes (IPv6) or 5 bytes
            b"d1:rd2:id20:mnopqrstuvwxyz1234566:valuesl5:abcdeee1:t2:aa1:y1:re",
            // announce with port 0 and no implied_port, and a bad token type
            b"d1:ad2:id20:abcdefghij01234567899:info_hash20:mnopqrstuvwxyz1234564:porti0e5:token1:xe1:q13:announce_peer1:t2:aa1:y1:qe",
            b"d1:ad2:id20:abcdefghij01234567899:info_hash20:mnopqrstuvwxyz1234564:porti1e5:tokeni1ee1:q13:announce_peer1:t2:aa1:y1:qe",
            // port out of range
            b"d1:ad2:id20:abcdefghij01234567899:info_hash20:mnopqrstuvwxyz1234564:porti70000e5:token1:xe1:q13:announce_peer1:t2:aa1:y1:qe",
            // unknown y, a list at top level
            b"d1:t2:aa1:y1:ze",
            b"l1:t2:aae",
        ];
        for b in bad {
            assert!(decode(b).is_err(), "{}", String::from_utf8_lossy(b));
        }
        assert_eq!(
            decode(&vec![b'x'; MAX_DATAGRAM + 1]),
            Err(DecodeError::TooLong)
        );
    }

    #[test]
    fn unknown_method_keeps_the_sender_id() {
        let wire = b"d1:ad2:id20:abcdefghij0123456789e1:q3:foo1:t2:aa1:y1:qe";
        match decode(wire).unwrap().body {
            Body::Query { id, query, .. } => {
                assert_eq!(&id, A);
                assert_eq!(query, Query::Unknown(b"foo".to_vec()));
            }
            b => panic!("{b:?}"),
        }
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    fn valid_wire() -> impl Strategy<Value = Vec<u8>> {
        (any::<[u8; 20]>(), any::<[u8; 20]>(), 0usize..6).prop_map(|(id, t, k)| {
            let body = match k {
                0 => Body::Query {
                    id,
                    query: Query::Ping,
                    ro: true,
                },
                1 => Body::Query {
                    id,
                    query: Query::SampleInfohashes { target: t },
                    ro: false,
                },
                2 => Body::Query {
                    id,
                    query: Query::AnnouncePeer {
                        info_hash: t,
                        port: 6881,
                        implied_port: true,
                        token: vec![1, 2, 3],
                    },
                    ro: false,
                },
                3 => Body::Response(Response {
                    id,
                    nodes: vec![CompactNode {
                        id: t,
                        addr: "1.2.3.4:5".parse().unwrap(),
                    }],
                    samples: Some(vec![t, id]),
                    interval: Some(60),
                    num: Some(2),
                    ..Default::default()
                }),
                4 => Body::Response(Response {
                    id,
                    values: vec!["4.3.2.1:80".parse().unwrap()],
                    token: Some(vec![9; 8]),
                    ..Default::default()
                }),
                _ => Body::Error {
                    code: 204,
                    message: "Method Unknown".into(),
                },
            };
            encode(&Message {
                tid: vec![t[0], t[1]],
                body,
                ip: Some("8.8.8.8:53".parse().unwrap()),
                version: Some(b"MG01".to_vec()),
            })
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(4096))]

        /// Arbitrary bytes never panic the decoder (docs/DHT.md §3.8).
        #[test]
        fn arbitrary_bytes_never_panic(buf in proptest::collection::vec(any::<u8>(), 0..3000)) {
            let _ = decode(&buf);
        }

        /// Valid messages with random byte flips, truncations and insertions never panic, and
        /// whatever decodes re-encodes and decodes to the same message.
        #[test]
        fn mutated_messages_never_panic(
            wire in valid_wire(),
            flips in proptest::collection::vec((any::<usize>(), any::<u8>()), 0..8),
            cut in any::<usize>(),
            insert in proptest::collection::vec(any::<u8>(), 0..8),
        ) {
            let mut w = wire.clone();
            prop_assert!(decode(&w).is_ok());
            for (i, b) in flips {
                let n = w.len();
                if n > 0 { w[i % n] = b; }
            }
            let at = cut % (w.len() + 1);
            let mut m = w[..at].to_vec();
            m.extend_from_slice(&insert);
            m.extend_from_slice(&w[at..]);
            for candidate in [&w[..], &m[..], &w[..at]] {
                if let Ok(msg) = decode(candidate) {
                    let again = decode(&encode(&msg));
                    prop_assert_eq!(again, Ok(msg));
                }
            }
        }

        /// Random bencode-shaped dicts with KRPC keys and wrong types never panic.
        #[test]
        fn krpc_shaped_garbage_never_panics(
            y in prop::sample::select(vec![&b"q"[..], b"r", b"e", b"x"]),
            q in prop::sample::select(vec![&b"ping"[..], b"find_node", b"get_peers", b"announce_peer", b"sample_infohashes", b"zz"]),
            field in proptest::collection::vec(any::<u8>(), 0..64),
            n in any::<i64>(),
        ) {
            let b = |s: &[u8]| Value::Bytes(s.to_vec());
            for key in [&b"id"[..], b"target", b"info_hash", b"port", b"token", b"nodes", b"values", b"samples", b"interval", b"implied_port"] {
                for val in [b(&field), Value::Int(n), Value::List(vec![b(&field)]), Value::Dict(vec![])] {
                    let inner = Value::Dict(vec![(b"id".to_vec(), b(&[1; 20])), (key.to_vec(), val.clone())]);
                    let msg = Value::Dict(vec![
                        (b"t".to_vec(), b(b"aa")),
                        (b"y".to_vec(), b(y)),
                        (b"q".to_vec(), b(q)),
                        (b"a".to_vec(), inner.clone()),
                        (b"r".to_vec(), inner),
                        (b"e".to_vec(), val),
                    ]);
                    let _ = decode(&bencode::encode(&msg));
                }
            }
        }
    }
}
