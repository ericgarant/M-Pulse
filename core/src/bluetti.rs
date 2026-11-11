//! Wire format of the Bluetti Elite 200 V2's BLE link.
//!
//! The power station speaks Modbus RTU over BLE (service `ff00`, write `ff02`,
//! notify `ff01`), wrapped in an encrypted channel that every newer Bluetti
//! ("IoT protocol V2") requires. Taken from the open-source `bluetti-bt-lib`
//! (which reads the AC180 and friends the same way) and the Bluetti Android
//! app v3.1.5 (`ProtocolParserV2.parseHomeData`, `ProtocolAddrV2`).
//!
//! ## Encrypted channel
//!
//! Before any Modbus traffic, the power station and the client agree a key:
//!
//! 1. The station sends a plain challenge `2A 2A 01 xx c0 c1 c2 c3 ck ck`.
//!    Both sides derive a temporary AES-128 key and IV from it, and the
//!    client answers `2A 2A 02 04 iv[8..12] ck ck`.
//! 2. The station sends its P-256 public key, signed with Bluetti's fixed
//!    key, encrypted under the temporary key.
//! 3. The client answers with its own fresh public key, signed with the
//!    app's fixed key, also under the temporary key.
//! 4. The station confirms, and from then on every message is AES-256-CBC
//!    under the ECDH shared secret, each with a random IV seed.
//!
//! The fixed keys below are the ones every Bluetti app ships with; they are
//! public in `bluetti-bt-lib`.
//!
//! ## Data
//!
//! Registers are 16-bit big-endian words. 32-bit values span two registers,
//! low word first. The app's home screen reads a block from register 100.

use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use md5::{Digest, Md5};
use p256::ecdsa::signature::{Signer, Verifier};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use p256::elliptic_curve::sec1::{FromEncodedPoint, ToEncodedPoint};
use p256::{EncodedPoint, PublicKey, SecretKey};

/// XOR-ed with the challenge's hash to make the temporary AES key.
const LOCAL_AES_KEY: [u8; 16] = hex16("459FC535808941F17091E0993EE3E93D");
/// Signs the client's public key (the app's private key).
const CLIENT_SIGNING_KEY: [u8; 32] =
    hex32("4F19A16E3E87BDD9BD24D3E5495B88041511943CBC8B969ADE9641D0F56AF337");
/// Checks the station's public key: the X and Y of Bluetti's signing key.
const STATION_VERIFY_X: [u8; 32] =
    hex32("A73ABF5D2232C8C1C72E68304343C272495E3A8FD6F30EA96DE2F4B3CE60B251");
const STATION_VERIFY_Y: [u8; 32] =
    hex32("EE21AC667CF8A71E18B46B664EAEFFE3C489F24F695B6411DB7E22CCC85A8594");

/// Marks a key-exchange message.
const KEX_MAGIC: [u8; 2] = [0x2A, 0x2A];
const MSG_CHALLENGE: u8 = 1;
const MSG_CHALLENGE_ACCEPTED: u8 = 3;
const MSG_PEER_PUBKEY: u8 = 4;
const MSG_PUBKEY_ACCEPTED: u8 = 6;

const BLOCK: usize = 16;

/// Why a message could not be handled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Too short, misaligned, or a length that does not add up.
    Malformed,
    /// A key-exchange message's checksum is wrong.
    Checksum,
    /// The station's public key is not signed by Bluetti.
    BadSignature,
    /// The station refused our public key.
    Refused,
    /// An encrypted message arrived before the temporary key existed.
    NoKey,
    /// The random bytes given for our key pair are not a valid key.
    BadRandom,
    /// A Modbus reply is short, has a bad CRC, or answers something else.
    BadReply,
    /// The station answered a Modbus request with an exception code.
    Exception(u8),
}

/// What the link should do with an incoming notification.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// Write these bytes to the station (part of the key exchange).
    Send(Vec<u8>),
    /// The key exchange finished; Modbus requests can be sent now.
    Ready,
    /// A decrypted Modbus reply.
    Data(Vec<u8>),
    /// Nothing to do (an acknowledgement, or part of a longer message).
    Nothing,
}

/// The encrypted channel's state for one connection.
///
/// Feed every notification to [`Channel::receive`] and write back whatever it
/// asks for. Once it reports [`Event::Ready`], wrap requests with
/// [`Channel::seal`].
pub struct Channel {
    temp_key: Option<[u8; 16]>,
    temp_iv: [u8; 16],
    own_secret: Option<SecretKey>,
    station_key: Option<PublicKey>,
    key: Option<[u8; 32]>,
    partial: Vec<u8>,
}

impl Default for Channel {
    fn default() -> Self {
        Self::new()
    }
}

impl Channel {
    pub fn new() -> Self {
        Self {
            temp_key: None,
            temp_iv: [0; 16],
            own_secret: None,
            station_key: None,
            key: None,
            partial: Vec::new(),
        }
    }

    /// Whether requests can be sent.
    pub fn is_ready(&self) -> bool {
        self.key.is_some()
    }

    /// Handles one notification.
    ///
    /// `random` must be 32 fresh random bytes. They become this connection's
    /// private key if the station's public key arrives in this notification;
    /// otherwise they are unused.
    pub fn receive(&mut self, data: &[u8], random: [u8; 32]) -> Result<Event, Error> {
        if self.partial.is_empty() && data.starts_with(&KEX_MAGIC) {
            return self.plain_kex(data);
        }
        // Encrypted messages may in principle span notifications.
        self.partial.extend_from_slice(data);
        let Some(total) = self.sealed_len() else {
            return Ok(Event::Nothing);
        };
        if self.partial.len() < total {
            return Ok(Event::Nothing);
        }
        let message = core::mem::take(&mut self.partial);
        if message.len() > total {
            return Err(Error::Malformed);
        }
        let plain = self.open(&message)?;
        if plain.starts_with(&KEX_MAGIC) && self.key.is_none() {
            return self.sealed_kex(&plain, random);
        }
        Ok(Event::Data(plain))
    }

    /// Encrypts a request for the station. `seed` must be 4 fresh random
    /// bytes. Returns `None` before the key exchange has finished.
    pub fn seal(&self, request: &[u8], seed: [u8; 4]) -> Option<Vec<u8>> {
        let key = self.key?;
        let iv = md5(&seed);
        let mut out = Vec::with_capacity(request.len() + 6 + BLOCK);
        out.extend_from_slice(&(request.len() as u16).to_be_bytes());
        out.extend_from_slice(&seed);
        out.extend(cbc_encrypt(Key::Aes256(&key), &iv, request));
        Some(out)
    }

    /// The full length of the encrypted message being collected, once its
    /// header is in.
    fn sealed_len(&self) -> Option<usize> {
        let len = usize::from(u16::from_be_bytes([
            *self.partial.first()?,
            *self.partial.get(1)?,
        ]));
        let header = if self.key.is_some() { 6 } else { 2 };
        Some(header + len.div_ceil(BLOCK) * BLOCK)
    }

    fn open(&self, message: &[u8]) -> Result<Vec<u8>, Error> {
        let len = usize::from(u16::from_be_bytes([message[0], message[1]]));
        let mut plain = match (&self.key, &self.temp_key) {
            (Some(key), _) => {
                let seed = message.get(2..6).ok_or(Error::Malformed)?;
                cbc_decrypt(Key::Aes256(key), &md5(seed), &message[6..])?
            }
            (None, Some(key)) => cbc_decrypt(Key::Aes128(key), &self.temp_iv, &message[2..])?,
            (None, None) => return Err(Error::NoKey),
        };
        if len > plain.len() {
            return Err(Error::Malformed);
        }
        plain.truncate(len);
        Ok(plain)
    }

    /// A key-exchange message sent in the clear: the challenge, or its
    /// acceptance.
    fn plain_kex(&mut self, data: &[u8]) -> Result<Event, Error> {
        let (kind, body) = kex_body(data)?;
        match kind {
            MSG_CHALLENGE => {
                let challenge: [u8; 4] = body.try_into().map_err(|_| Error::Malformed)?;
                let mut reversed = challenge;
                reversed.reverse();
                self.temp_iv = md5(&reversed);
                let mut key = [0u8; 16];
                for (k, (a, b)) in key.iter_mut().zip(self.temp_iv.iter().zip(LOCAL_AES_KEY)) {
                    *k = a ^ b;
                }
                self.temp_key = Some(key);
                self.own_secret = None;
                self.station_key = None;
                self.key = None;
                let mut reply = vec![0x02, 0x04];
                reply.extend_from_slice(&self.temp_iv[8..12]);
                Ok(Event::Send(kex_message(&reply)))
            }
            MSG_CHALLENGE_ACCEPTED => Ok(Event::Nothing),
            _ => Err(Error::Malformed),
        }
    }

    /// A key-exchange message sent under the temporary key: the station's
    /// public key, or its acceptance of ours.
    fn sealed_kex(&mut self, plain: &[u8], random: [u8; 32]) -> Result<Event, Error> {
        let (kind, body) = kex_body(plain)?;
        match kind {
            MSG_PEER_PUBKEY => {
                if body.len() != 128 {
                    return Err(Error::Malformed);
                }
                let (point, signature) = body.split_at(64);
                let station = public_key(point)?;
                // The station repeats its key until it hears back. Answering
                // a repeat would replace our key pair after the station has
                // already taken the first one, so repeats are ignored.
                if self.station_key == Some(station) {
                    return Ok(Event::Nothing);
                }
                verify_station(point, signature, &self.temp_iv)?;

                let secret = SecretKey::from_slice(&random).map_err(|_| Error::BadRandom)?;
                let own_point = secret.public_key().to_encoded_point(false);
                let own_xy = &own_point.as_bytes()[1..];
                let mut to_sign = own_xy.to_vec();
                to_sign.extend_from_slice(&self.temp_iv);
                let signer =
                    SigningKey::from_slice(&CLIENT_SIGNING_KEY).map_err(|_| Error::BadRandom)?;
                let signature: Signature = signer.sign(&to_sign);

                let mut reply = vec![0x05, 0x80];
                reply.extend_from_slice(own_xy);
                reply.extend_from_slice(&signature.to_bytes());
                let message = kex_message(&reply);

                self.own_secret = Some(secret);
                self.station_key = Some(station);
                let key = self.temp_key.ok_or(Error::NoKey)?;
                let mut out = (message.len() as u16).to_be_bytes().to_vec();
                out.extend(cbc_encrypt(Key::Aes128(&key), &self.temp_iv, &message));
                Ok(Event::Send(out))
            }
            MSG_PUBKEY_ACCEPTED => {
                if body != [0] {
                    return Err(Error::Refused);
                }
                let (Some(secret), Some(station)) = (&self.own_secret, &self.station_key) else {
                    return Err(Error::Malformed);
                };
                let shared =
                    p256::ecdh::diffie_hellman(secret.to_nonzero_scalar(), station.as_affine());
                let mut key = [0u8; 32];
                key.copy_from_slice(shared.raw_secret_bytes());
                self.key = Some(key);
                Ok(Event::Ready)
            }
            _ => Err(Error::Malformed),
        }
    }
}

/// Splits `2A 2A kind x body.. ck ck` into the kind and body, checking the sum.
fn kex_body(data: &[u8]) -> Result<(u8, &[u8]), Error> {
    if data.len() < 6 || !data.starts_with(&KEX_MAGIC) {
        return Err(Error::Malformed);
    }
    let inner = &data[2..data.len() - 2];
    let sum = u16::from_be_bytes([data[data.len() - 2], data[data.len() - 1]]);
    if sum16(inner) != sum {
        return Err(Error::Checksum);
    }
    Ok((inner[0], &inner[2..]))
}

fn kex_message(inner: &[u8]) -> Vec<u8> {
    let mut out = KEX_MAGIC.to_vec();
    out.extend_from_slice(inner);
    out.extend_from_slice(&sum16(inner).to_be_bytes());
    out
}

fn sum16(bytes: &[u8]) -> u16 {
    bytes
        .iter()
        .fold(0u16, |s, b| s.wrapping_add(u16::from(*b)))
}

fn md5(bytes: &[u8]) -> [u8; 16] {
    Md5::digest(bytes).into()
}

fn public_key(xy: &[u8]) -> Result<PublicKey, Error> {
    let x: &[u8; 32] = xy[..32].try_into().map_err(|_| Error::Malformed)?;
    let y: &[u8; 32] = xy[32..].try_into().map_err(|_| Error::Malformed)?;
    let point = EncodedPoint::from_affine_coordinates(x.into(), y.into(), false);
    Option::from(PublicKey::from_encoded_point(&point)).ok_or(Error::Malformed)
}

fn verify_station(point: &[u8], signature: &[u8], iv: &[u8; 16]) -> Result<(), Error> {
    let bluetti = EncodedPoint::from_affine_coordinates(
        (&STATION_VERIFY_X).into(),
        (&STATION_VERIFY_Y).into(),
        false,
    );
    let verifier = VerifyingKey::from_encoded_point(&bluetti).map_err(|_| Error::BadSignature)?;
    let signature = Signature::from_slice(signature).map_err(|_| Error::BadSignature)?;
    let mut signed = point.to_vec();
    signed.extend_from_slice(iv);
    verifier
        .verify(&signed, &signature)
        .map_err(|_| Error::BadSignature)
}

enum Key<'a> {
    Aes128(&'a [u8; 16]),
    Aes256(&'a [u8; 32]),
}

/// AES-CBC with zero padding up to a whole block, as the station expects.
fn cbc_encrypt(key: Key<'_>, iv: &[u8; 16], data: &[u8]) -> Vec<u8> {
    let mut out = data.to_vec();
    out.resize(data.len().div_ceil(BLOCK) * BLOCK, 0);
    let mut prev = *iv;
    for chunk in out.as_chunks_mut::<BLOCK>().0 {
        for (c, p) in chunk.iter_mut().zip(prev) {
            *c ^= p;
        }
        let mut block = aes::Block::from(*chunk);
        match key {
            Key::Aes128(k) => aes::Aes128::new(k.into()).encrypt_block(&mut block),
            Key::Aes256(k) => aes::Aes256::new(k.into()).encrypt_block(&mut block),
        }
        chunk.copy_from_slice(&block);
        prev = *chunk;
    }
    out
}

fn cbc_decrypt(key: Key<'_>, iv: &[u8; 16], data: &[u8]) -> Result<Vec<u8>, Error> {
    if data.is_empty() || !data.len().is_multiple_of(BLOCK) {
        return Err(Error::Malformed);
    }
    let mut out = data.to_vec();
    let mut prev = *iv;
    for chunk in out.as_chunks_mut::<BLOCK>().0 {
        let cipher = *chunk;
        let mut block = aes::Block::from(*chunk);
        match key {
            Key::Aes128(k) => aes::Aes128::new(k.into()).decrypt_block(&mut block),
            Key::Aes256(k) => aes::Aes256::new(k.into()).decrypt_block(&mut block),
        }
        chunk.copy_from_slice(&block);
        for (c, p) in chunk.iter_mut().zip(prev) {
            *c ^= p;
        }
        prev = cipher;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Modbus

/// Modbus CRC-16 (polynomial 0xA001, start 0xFFFF).
pub fn crc16(bytes: &[u8]) -> u16 {
    bytes.iter().fold(0xFFFF, |mut crc, b| {
        crc ^= u16::from(*b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xA001
            } else {
                crc >> 1
            };
        }
        crc
    })
}

/// "Read holding registers" (function 3) for `count` registers from `start`.
pub fn read_request(start: u16, count: u16) -> Vec<u8> {
    let mut out = vec![0x01, 0x03];
    out.extend_from_slice(&start.to_be_bytes());
    out.extend_from_slice(&count.to_be_bytes());
    let crc = crc16(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    out
}

/// The register bytes of a reply to a `count`-register read.
pub fn read_reply(reply: &[u8], count: u16) -> Result<&[u8], Error> {
    if reply.len() >= 5 && reply[1] == 0x83 {
        return Err(Error::Exception(reply[2]));
    }
    let want = usize::from(count) * 2;
    if reply.len() != want + 5 || reply[1] != 0x03 || usize::from(reply[2]) != want {
        return Err(Error::BadReply);
    }
    let crc = u16::from_le_bytes([reply[reply.len() - 2], reply[reply.len() - 1]]);
    if crc16(&reply[..reply.len() - 2]) != crc {
        return Err(Error::BadReply);
    }
    Ok(&reply[3..3 + want])
}

// ---------------------------------------------------------------------------
// Home data

/// First register of the app's home-screen block.
pub const HOME_START: u16 = 100;
/// Registers read from [`HOME_START`]: up to the inverter status at 161.
pub const HOME_COUNT: u16 = 62;

/// What the power station is doing, from the home block.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    /// Model as the station reports it, e.g. "EL200V2".
    pub model: String,
    /// State of charge, 0-100 %.
    pub soc_pct: u8,
    /// Battery pack voltage.
    pub pack_volts: f32,
    /// Battery pack current; positive while discharging.
    pub pack_amps: f32,
    /// Raw charging-status code: 0 idle, 2 discharging (others unconfirmed).
    pub charging_status: u16,
    /// Minutes to full while charging, as reported.
    pub minutes_to_full: u16,
    /// Minutes to empty while discharging, as reported.
    pub minutes_to_empty: u16,
    /// Total DC output (12 V, USB, car port), watts.
    pub dc_out_w: u32,
    /// Total AC output, watts.
    pub ac_out_w: i32,
    /// Solar input, watts.
    pub pv_in_w: u32,
    /// Grid (AC) input, watts.
    pub grid_in_w: i32,
    /// Whether the AC outlets are switched on.
    pub ac_on: bool,
    /// Whether the DC outputs are switched on.
    pub dc_on: bool,
}

impl Status {
    /// Reads the home block (`HOME_COUNT` registers from `HOME_START`).
    pub fn parse(regs: &[u8]) -> Option<Self> {
        if regs.len() < usize::from(HOME_COUNT) * 2 {
            return None;
        }
        let u16_at = |b: usize| u16::from_be_bytes([regs[b], regs[b + 1]]);
        // 32-bit values: low word first.
        let u32_at = |b: usize| u32::from(u16_at(b)) | (u32::from(u16_at(b + 2)) << 16);
        let model: String = regs[20..32]
            .as_chunks::<2>()
            .0
            .iter()
            // Each register holds two characters, swapped.
            .flat_map(|&[a, b]| [b, a])
            .filter(|c| c.is_ascii_graphic())
            .map(char::from)
            .collect();
        let ctrl = u16_at(48);
        Some(Self {
            model,
            soc_pct: u16_at(4).min(100) as u8,
            // The app divides by 10, but the Elite 200 V2 reports hundredths:
            // 3966 with 2.2 A out matched the 83 W the AC outlet was drawing.
            pack_volts: f32::from(u16_at(0)) / 100.0,
            pack_amps: f32::from(u16_at(2)) / 10.0,
            charging_status: u16_at(6),
            minutes_to_full: u16_at(8),
            minutes_to_empty: u16_at(10),
            dc_out_w: u32_at(80),
            ac_out_w: u32_at(84) as i32,
            pv_in_w: u32_at(88),
            grid_in_w: u32_at(92) as i32,
            ac_on: ctrl & (1 << 1) != 0,
            dc_on: ctrl & (1 << 2) != 0,
        })
    }
}

// ---------------------------------------------------------------------------

const fn hex_digit(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => panic!("not a hex digit"),
    }
}

const fn hex16(s: &str) -> [u8; 16] {
    let b = s.as_bytes();
    let mut out = [0u8; 16];
    let mut i = 0;
    while i < 16 {
        out[i] = hex_digit(b[2 * i]) << 4 | hex_digit(b[2 * i + 1]);
        i += 1;
    }
    out
}

const fn hex32(s: &str) -> [u8; 32] {
    let b = s.as_bytes();
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = hex_digit(b[2 * i]) << 4 | hex_digit(b[2 * i + 1]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_modbus() {
        // 01 03 00 64 00 3E -> CRC from the standard Modbus calculator.
        let req = read_request(100, 62);
        assert_eq!(&req[..6], &[0x01, 0x03, 0x00, 0x64, 0x00, 0x3E]);
        assert_eq!(crc16(&req), 0, "CRC over a frame with its CRC is zero");
    }

    #[test]
    fn read_reply_checks_length_and_crc() {
        let mut reply = vec![0x01, 0x03, 0x04, 0x00, 0x2A, 0x01, 0x00];
        let crc = crc16(&reply);
        reply.extend_from_slice(&crc.to_le_bytes());
        assert_eq!(read_reply(&reply, 2), Ok(&[0x00, 0x2A, 0x01, 0x00][..]));
        assert_eq!(read_reply(&reply, 3), Err(Error::BadReply));
        let last = reply.len() - 1;
        reply[last] ^= 1;
        assert_eq!(read_reply(&reply, 2), Err(Error::BadReply));
        let mut exc = vec![0x01, 0x83, 0x02];
        let crc = crc16(&exc);
        exc.extend_from_slice(&crc.to_le_bytes());
        assert_eq!(read_reply(&exc, 2), Err(Error::Exception(2)));
    }

    #[test]
    fn cbc_round_trip_pads_with_zeros() {
        let key = [7u8; 32];
        let iv = [3u8; 16];
        let sealed = cbc_encrypt(Key::Aes256(&key), &iv, b"hello bluetti");
        assert_eq!(sealed.len(), 16);
        let opened = cbc_decrypt(Key::Aes256(&key), &iv, &sealed).unwrap();
        assert_eq!(&opened[..13], b"hello bluetti");
        assert!(opened[13..].iter().all(|b| *b == 0));
    }

    #[test]
    fn aes128_matches_fips197() {
        // FIPS-197 appendix C.1, as a one-block CBC with a zero IV.
        let key = hex16("000102030405060708090a0b0c0d0e0f");
        let plain = hex16("00112233445566778899aabbccddeeff");
        let sealed = cbc_encrypt(Key::Aes128(&key), &[0; 16], &plain);
        assert_eq!(sealed, hex16("69c4e0d86a7b0430d8cdb78070b4c55a"));
    }

    #[test]
    fn challenge_reply_matches_reference() {
        // Challenge 01 02 03 04. The reference library derives
        // iv = md5(04 03 02 01) and answers with iv[8..12].
        let challenge = kex_message(&[0x01, 0x04, 0x01, 0x02, 0x03, 0x04]);
        let mut ch = Channel::new();
        let Event::Send(reply) = ch.receive(&challenge, [1; 32]).unwrap() else {
            panic!("expected a reply");
        };
        let iv = md5(&[4, 3, 2, 1]);
        let mut want = vec![0x02, 0x04];
        want.extend_from_slice(&iv[8..12]);
        assert_eq!(reply, kex_message(&want));
        assert!(!ch.is_ready());
    }

    #[test]
    fn rejects_bad_checksum() {
        let mut msg = kex_message(&[0x01, 0x04, 0x01, 0x02, 0x03, 0x04]);
        let last = msg.len() - 1;
        msg[last] ^= 1;
        assert_eq!(Channel::new().receive(&msg, [1; 32]), Err(Error::Checksum));
    }

    /// Plays the station's side of the exchange with a made-up station key
    /// signed by a test key, so only the signature check is bypassed.
    #[test]
    fn full_exchange_with_simulated_station() {
        let mut ch = Channel::new();
        let challenge = kex_message(&[0x01, 0x04, 0xDE, 0xAD, 0xBE, 0xEF]);
        assert!(matches!(
            ch.receive(&challenge, [0; 32]),
            Ok(Event::Send(_))
        ));
        let accepted = kex_message(&[0x03, 0x01, 0x00]);
        assert_eq!(ch.receive(&accepted, [0; 32]), Ok(Event::Nothing));

        // Skip the signature: install the station's key directly, then send
        // our own public key's acceptance.
        let station_secret = SecretKey::from_slice(&[0x42; 32]).unwrap();
        let own_secret = SecretKey::from_slice(&[0x24; 32]).unwrap();
        ch.station_key = Some(station_secret.public_key());
        ch.own_secret = Some(own_secret.clone());
        let ok = kex_message(&[0x06, 0x01, 0x00]);
        let temp_key = ch.temp_key.unwrap();
        let mut sealed = (ok.len() as u16).to_be_bytes().to_vec();
        sealed.extend(cbc_encrypt(Key::Aes128(&temp_key), &ch.temp_iv, &ok));
        assert_eq!(ch.receive(&sealed, [0; 32]), Ok(Event::Ready));
        assert!(ch.is_ready());

        // The station computes the same key from its side.
        let station_shared = p256::ecdh::diffie_hellman(
            station_secret.to_nonzero_scalar(),
            own_secret.public_key().as_affine(),
        );
        let key: [u8; 32] = (*station_shared.raw_secret_bytes()).into();

        // A request sealed by us opens on the station's side.
        let req = read_request(HOME_START, HOME_COUNT);
        let sealed = ch.seal(&req, [9, 8, 7, 6]).unwrap();
        let opened = cbc_decrypt(Key::Aes256(&key), &md5(&[9, 8, 7, 6]), &sealed[6..]).unwrap();
        assert_eq!(&opened[..req.len()], &req[..]);

        // A reply sealed by the station opens on ours, even split in two.
        let mut reply = vec![0x01, 0x03, 0x02, 0x00, 0x55];
        let crc = crc16(&reply);
        reply.extend_from_slice(&crc.to_le_bytes());
        let mut msg = (reply.len() as u16).to_be_bytes().to_vec();
        msg.extend_from_slice(&[1, 2, 3, 4]);
        msg.extend(cbc_encrypt(Key::Aes256(&key), &md5(&[1, 2, 3, 4]), &reply));
        let (a, b) = msg.split_at(5);
        assert_eq!(ch.receive(a, [0; 32]), Ok(Event::Nothing));
        assert_eq!(ch.receive(b, [0; 32]), Ok(Event::Data(reply)));
    }

    #[test]
    fn station_key_needs_bluetti_signature() {
        let mut ch = Channel::new();
        let challenge = kex_message(&[0x01, 0x04, 0x01, 0x02, 0x03, 0x04]);
        ch.receive(&challenge, [0; 32]).unwrap();
        let fake = SecretKey::from_slice(&[0x42; 32]).unwrap().public_key();
        let mut body = vec![0x04, 0x80];
        body.extend_from_slice(&fake.to_encoded_point(false).as_bytes()[1..]);
        body.extend_from_slice(&[0x11; 64]);
        let msg = kex_message(&body);
        let temp_key = ch.temp_key.unwrap();
        let mut sealed = (msg.len() as u16).to_be_bytes().to_vec();
        sealed.extend(cbc_encrypt(Key::Aes128(&temp_key), &ch.temp_iv, &msg));
        assert_eq!(ch.receive(&sealed, [5; 32]), Err(Error::BadSignature));
    }

    #[test]
    fn ignores_repeated_station_key() {
        let mut ch = Channel::new();
        let challenge = kex_message(&[0x01, 0x04, 0x01, 0x02, 0x03, 0x04]);
        ch.receive(&challenge, [0; 32]).unwrap();
        let station = SecretKey::from_slice(&[0x42; 32]).unwrap().public_key();
        ch.station_key = Some(station);
        ch.own_secret = Some(SecretKey::from_slice(&[0x24; 32]).unwrap());
        let mut body = vec![0x04, 0x80];
        body.extend_from_slice(&station.to_encoded_point(false).as_bytes()[1..]);
        body.extend_from_slice(&[0x11; 64]); // signature never checked for a repeat
        let msg = kex_message(&body);
        let temp_key = ch.temp_key.unwrap();
        let mut sealed = (msg.len() as u16).to_be_bytes().to_vec();
        sealed.extend(cbc_encrypt(Key::Aes128(&temp_key), &ch.temp_iv, &msg));
        assert_eq!(ch.receive(&sealed, [5; 32]), Ok(Event::Nothing));
        assert_eq!(
            ch.own_secret.as_ref().map(|s| s.to_bytes()),
            Some(SecretKey::from_slice(&[0x24; 32]).unwrap().to_bytes())
        );
    }

    #[test]
    fn signs_own_key_verifiably() {
        // The station checks our signature with the app's public key.
        let signer = SigningKey::from_slice(&CLIENT_SIGNING_KEY).unwrap();
        let sig: Signature = signer.sign(b"abc");
        assert!(signer.verifying_key().verify(b"abc", &sig).is_ok());
    }

    #[test]
    fn parses_home_block() {
        let mut regs = vec![0u8; usize::from(HOME_COUNT) * 2];
        let put = |r: &mut Vec<u8>, b: usize, v: u16| r[b..b + 2].copy_from_slice(&v.to_be_bytes());
        put(&mut regs, 0, 3966); // 39.66 V
        put(&mut regs, 4, 87); // 87 %
                               // "EL200V2" with each register's bytes swapped.
        let name = b"EL200V2\0\0\0\0\0";
        for (i, pair) in name.as_chunks::<2>().0.iter().enumerate() {
            regs[20 + 2 * i] = pair[1];
            regs[21 + 2 * i] = pair[0];
        }
        put(&mut regs, 48, 0b111); // power, AC, DC on
        put(&mut regs, 80, 45); // DC out 45 W (low word)
        put(&mut regs, 84, 120); // AC out 120 W
        put(&mut regs, 88, 0x86A0); // PV 100 000 W, low word
        put(&mut regs, 90, 0x0001); // ... high word
        let s = Status::parse(&regs).unwrap();
        assert_eq!(s.model, "EL200V2");
        assert_eq!(s.soc_pct, 87);
        assert!((s.pack_volts - 39.66).abs() < 1e-3);
        assert!(s.ac_on && s.dc_on);
        assert_eq!(s.dc_out_w, 45);
        assert_eq!(s.ac_out_w, 120);
        assert_eq!(s.pv_in_w, 100_000);
        assert!(Status::parse(&regs[..10]).is_none());
    }
}
