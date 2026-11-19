//! Negentropy (NIP-77) responder of the server's `/relay` endpoint (D13–D15, ARCHITECTURE.md
//! §7). The phone-side client (`sync_up`) went with the phone database (D79) and was deleted
//! in D154.

use negentropy::{Id, Negentropy, NegentropyStorageVector};

use crate::error::Error;

pub const FRAME_SIZE_LIMIT: u64 = 60_000;

/// Server side: answer one NEG-OPEN/NEG-MSG. Used by the server relay endpoint.
pub struct NegResponder {
    inner: Negentropy<'static, NegentropyStorageVector>,
}

impl NegResponder {
    /// Build from (created_at, id) items; errors on bad frame limit.
    pub fn new(items: Vec<(u64, [u8; 32])>) -> crate::Result<Self> {
        let neg_err = |e: negentropy::Error| Error::Invalid(format!("negentropy: {e}"));
        let mut storage = NegentropyStorageVector::with_capacity(items.len());
        for (ts, id) in items {
            storage
                .insert(ts, Id::from_byte_array(id))
                .map_err(neg_err)?;
        }
        storage.seal().map_err(neg_err)?;
        Ok(Self {
            inner: Negentropy::owned(storage, FRAME_SIZE_LIMIT).map_err(neg_err)?,
        })
    }
    /// Hex in, hex out.
    pub fn reconcile_hex(&mut self, msg_hex: &str) -> crate::Result<String> {
        let msg = hex::decode(msg_hex).map_err(|e| Error::Invalid(format!("bad hex: {e}")))?;
        let reply = self
            .inner
            .reconcile(&msg)
            .map_err(|e| Error::Invalid(format!("negentropy: {e}")))?;
        Ok(hex::encode(reply))
    }
}
