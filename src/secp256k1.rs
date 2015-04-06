// Bitcoin secp256k1 bindings
// Written in 2014 by
//   Dawid Ciężarkiewicz
//   Andrew Poelstra
//
// To the extent possible under law, the author(s) have dedicated all
// copyright and related and neighboring rights to this software to
// the public domain worldwide. This software is distributed without
// any warranty.
//
// You should have received a copy of the CC0 Public Domain Dedication
// along with this software.
// If not, see <http://creativecommons.org/publicdomain/zero/1.0/>.
//

//! # Secp256k1
//! Rust bindings for Pieter Wuille's secp256k1 library, which is used for
//! fast and accurate manipulation of ECDSA signatures on the secp256k1
//! curve. Such signatures are used extensively by the Bitcoin network
//! and its derivatives.
//!

#![crate_type = "lib"]
#![crate_type = "rlib"]
#![crate_type = "dylib"]
#![crate_name = "secp256k1"]

// Keep this until 1.0 I guess; it's needed for `black_box` at least
#![cfg_attr(test, feature(test))]

// Coding conventions
#![deny(non_upper_case_globals)]
#![deny(non_camel_case_types)]
#![deny(non_snake_case)]
#![deny(unused_mut)]
#![warn(missing_docs)]

extern crate crypto;
extern crate rustc_serialize as serialize;
#[cfg(test)] extern crate test;

extern crate libc;
extern crate rand;

use std::intrinsics::copy_nonoverlapping;
use std::{fmt, io, ops, ptr};
use std::sync::{Once, ONCE_INIT};
use libc::c_int;
use rand::{OsRng, Rng, SeedableRng};

use crypto::fortuna::Fortuna;

#[macro_use]
mod macros;
pub mod constants;
pub mod ffi;
pub mod key;

/// I dunno where else to put this..
fn assert_type_is_copy<T: Copy>() { }

/// A tag used for recovering the public key from a compact signature
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct RecoveryId(i32);

/// An ECDSA signature
#[derive(Copy)]
pub struct Signature(usize, [u8; constants::MAX_SIGNATURE_SIZE]);

impl Signature {
    /// Converts the signature to a raw pointer suitable for use
    /// with the FFI functions
    #[inline]
    pub fn as_ptr(&self) -> *const u8 {
        let &Signature(_, ref data) = self;
        data.as_ptr()
    }

    /// Converts the signature to a mutable raw pointer suitable for use
    /// with the FFI functions
    #[inline]
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        let &mut Signature(_, ref mut data) = self;
        data.as_mut_ptr()
    }

    /// Returns the length of the signature
    #[inline]
    pub fn len(&self) -> usize {
        let &Signature(len, _) = self;
        len
    }

    /// Converts a byte slice to a signature
    #[inline]
    pub fn from_slice(data: &[u8]) -> Result<Signature, Error> {
        if data.len() <= constants::MAX_SIGNATURE_SIZE {
            let mut ret = [0; constants::MAX_SIGNATURE_SIZE];
            unsafe {
                copy_nonoverlapping(data.as_ptr(),
                                    ret.as_mut_ptr(),
                                    data.len());
            }
            Ok(Signature(data.len(), ret))
        } else {
            Err(Error::InvalidSignature)
        }
    }
}

impl ops::Index<usize> for Signature {
    type Output = u8;

    #[inline]
    fn index(&self, index: usize) -> &u8 {
        let &Signature(_, ref dat) = self;
        &dat[index]
    }
}

impl ops::Index<ops::Range<usize>> for Signature {
    type Output = [u8];

    #[inline]
    fn index(&self, index: ops::Range<usize>) -> &[u8] {
        let &Signature(_, ref dat) = self;
        &dat[index.start..index.end]
    }
}

impl ops::Index<ops::RangeFrom<usize>> for Signature {
    type Output = [u8];

    #[inline]
    fn index(&self, index: ops::RangeFrom<usize>) -> &[u8] {
        let &Signature(_, ref dat) = self;
        &dat[index.start..]
    }
}

impl ops::Index<ops::RangeFull> for Signature {
    type Output = [u8];

    #[inline]
    fn index(&self, _: ops::RangeFull) -> &[u8] {
        let &Signature(_, ref dat) = self;
        &dat[..]
    }
}

impl Clone for Signature {
    #[inline]
    fn clone(&self) -> Signature {
        unsafe {
            use std::mem;
            let mut ret: Signature = mem::uninitialized();
            copy_nonoverlapping(self.as_ptr(),
                                ret.as_mut_ptr(),
                                mem::size_of::<Signature>());
            ret
        }
    }
}

/// A (hashed) message input to an ECDSA signature
pub struct Message([u8; constants::MESSAGE_SIZE]);
impl_array_newtype!(Message, u8, constants::MESSAGE_SIZE);

impl Message {
    /// Converts a `MESSAGE_SIZE`-byte slice to a nonce
    #[inline]
    pub fn from_slice(data: &[u8]) -> Result<Message, Error> {
        match data.len() {
            constants::MESSAGE_SIZE => {
                let mut ret = [0; constants::MESSAGE_SIZE];
                unsafe {
                    copy_nonoverlapping(data.as_ptr(),
                                        ret.as_mut_ptr(),
                                        data.len());
                }
                Ok(Message(ret))
            }
            _ => Err(Error::InvalidMessage)
        }
    }
}

/// An ECDSA error
#[derive(Copy, PartialEq, Eq, Clone, Debug)]
pub enum Error {
    /// Signature failed verification
    IncorrectSignature,
    /// Badly sized message
    InvalidMessage,
    /// Bad public key
    InvalidPublicKey,
    /// Bad signature
    InvalidSignature,
    /// Bad secret key
    InvalidSecretKey,
    /// Signing failed: bad nonce, bad privkey or signature was too small
    SignFailed,
    /// Boolean-returning function returned the wrong boolean
    Unknown
}

// Passthrough Debug to Display, since errors should be user-visible
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        fmt::Debug::fmt(self, f)
    }
}

static mut Secp256k1_init: Once = ONCE_INIT;

/// The secp256k1 engine, used to execute all signature operations
pub struct Secp256k1 {
    rng: Fortuna
}

/// Does one-time initialization of the secp256k1 engine. Can be called
/// multiple times, and is called by the `Secp256k1` constructor. This
/// only needs to be called directly if you are using the library without
/// a `Secp256k1` object, e.g. batch key generation through
/// `key::PublicKey::from_secret_key`.
pub fn init() {
    unsafe {
        Secp256k1_init.call_once(|| {
            ffi::secp256k1_start(ffi::SECP256K1_START_VERIFY |
                                 ffi::SECP256K1_START_SIGN);
        });
    }
}

impl Secp256k1 {
    /// Constructs a new secp256k1 engine.
    pub fn new() -> io::Result<Secp256k1> {
        init();
        let mut osrng = try!(OsRng::new());
        let mut seed = [0; 2048];
        osrng.fill_bytes(&mut seed);
        Ok(Secp256k1 { rng: SeedableRng::from_seed(&seed[..]) })
    }

    /// Generates a random keypair. Convenience function for `key::SecretKey::new`
    /// and `key::PublicKey::from_secret_key`; call those functions directly for
    /// batch key generation.
    #[inline]
    pub fn generate_keypair(&mut self, compressed: bool)
                            -> (key::SecretKey, key::PublicKey) {
        let sk = key::SecretKey::new(&mut self.rng);
        let pk = key::PublicKey::from_secret_key(&sk, compressed);
        (sk, pk)
    }

    /// Constructs a signature for `msg` using the secret key `sk` and nonce `nonce`
    pub fn sign(&self, msg: &Message, sk: &key::SecretKey)
                -> Result<Signature, Error> {
        let mut sig = [0; constants::MAX_SIGNATURE_SIZE];
        let mut len = constants::MAX_SIGNATURE_SIZE as c_int;
        unsafe {
            if ffi::secp256k1_ecdsa_sign(msg.as_ptr(), (&mut sig).as_mut_ptr(),
                                         &mut len, sk.as_ptr(),
                                         ffi::secp256k1_nonce_function_rfc6979,
                                         ptr::null()) != 1 {
                return Err(Error::SignFailed);
            }
            // This assertation is probably too late :)
            assert!(len as usize <= constants::MAX_SIGNATURE_SIZE);
        };
        Ok(Signature(len as usize, sig))
    }

    /// Constructs a compact signature for `msg` using the secret key `sk`
    pub fn sign_compact(&self, msg: &Message, sk: &key::SecretKey)
                        -> Result<(Signature, RecoveryId), Error> {
        let mut sig = [0; constants::MAX_SIGNATURE_SIZE];
        let mut recid = 0;
        unsafe {
            if ffi::secp256k1_ecdsa_sign_compact(msg.as_ptr(),
                                                 sig.as_mut_ptr(), sk.as_ptr(),
                                                 ffi::secp256k1_nonce_function_default,
                                                 ptr::null(), &mut recid) != 1 {
                return Err(Error::SignFailed);
            }
        };
        Ok((Signature(constants::MAX_COMPACT_SIGNATURE_SIZE, sig), RecoveryId(recid)))
    }

    /// Determines the public key for which `sig` is a valid signature for
    /// `msg`. Returns through the out-pointer `pubkey`.
    pub fn recover_compact(&self, msg: &Message, sig: &[u8],
                           compressed: bool, recid: RecoveryId)
                            -> Result<key::PublicKey, Error> {
        let mut pk = key::PublicKey::new(compressed);
        let RecoveryId(recid) = recid;

        unsafe {
            let mut len = 0;
            if ffi::secp256k1_ecdsa_recover_compact(msg.as_ptr(),
                                                    sig.as_ptr(), pk.as_mut_ptr(), &mut len,
                                                    if compressed {1} else {0},
                                                    recid) != 1 {
                return Err(Error::InvalidSignature);
            }
            assert_eq!(len as usize, pk.len());
        };
        Ok(pk)
    }

    /// Checks that `sig` is a valid ECDSA signature for `msg` using the public
    /// key `pubkey`. Returns `Ok(true)` on success. Note that this function cannot
    /// be used for Bitcoin consensus checking since there are transactions out
    /// there with zero-padded signatures that don't fit in the `Signature` type.
    /// Use `verify_raw` instead.
    #[inline]
    pub fn verify(msg: &Message, sig: &Signature, pk: &key::PublicKey) -> Result<(), Error> {
        Secp256k1::verify_raw(msg, &sig[..], pk)
    }

    /// Checks that `sig` is a valid ECDSA signature for `msg` using the public
    /// key `pubkey`. Returns `Ok(true)` on success.
    #[inline]
    pub fn verify_raw(msg: &Message, sig: &[u8], pk: &key::PublicKey) -> Result<(), Error> {
        init();  // This is a static function, so we have to init
        let res = unsafe {
            ffi::secp256k1_ecdsa_verify(msg.as_ptr(),
                                        sig.as_ptr(), sig.len() as c_int,
                                        pk.as_ptr(), pk.len() as c_int)
        };

        match res {
            1 => Ok(()),
            0 => Err(Error::IncorrectSignature),
            -1 => Err(Error::InvalidPublicKey),
            -2 => Err(Error::InvalidSignature),
            _ => unreachable!()
        }
    }
}


#[cfg(test)]
mod tests {
    use std::iter::repeat;
    use rand::{Rng, thread_rng};

    use test::{Bencher, black_box};

    use key::PublicKey;
    use super::{Secp256k1, Signature, Message};
    use super::Error::{InvalidPublicKey, IncorrectSignature, InvalidSignature};

    #[test]
    fn invalid_pubkey() {
        let sig = Signature::from_slice(&[0; 72]).unwrap();
        let pk = PublicKey::new(true);
        let mut msg = [0u8; 32];
        thread_rng().fill_bytes(&mut msg);
        let msg = Message::from_slice(&msg).unwrap();

        assert_eq!(Secp256k1::verify(&msg, &sig, &pk), Err(InvalidPublicKey));
    }

    #[test]
    fn valid_pubkey_uncompressed() {
        let mut s = Secp256k1::new().unwrap();

        let (_, pk) = s.generate_keypair(false);

        let sig = Signature::from_slice(&[0; 72]).unwrap();
        let mut msg = [0u8; 32];
        thread_rng().fill_bytes(&mut msg);
        let msg = Message::from_slice(&msg).unwrap();

        assert_eq!(Secp256k1::verify(&msg, &sig, &pk), Err(InvalidSignature));
    }

    #[test]
    fn valid_pubkey_compressed() {
        let mut s = Secp256k1::new().unwrap();

        let (_, pk) = s.generate_keypair(true);
        let sig = Signature::from_slice(&[0; 72]).unwrap();
        let mut msg = [0u8; 32];
        thread_rng().fill_bytes(&mut msg);
        let msg = Message::from_slice(&msg).unwrap();

        assert_eq!(Secp256k1::verify(&msg, &sig, &pk), Err(InvalidSignature));
    }

    #[test]
    fn sign() {
        let mut s = Secp256k1::new().unwrap();

        let mut msg = [0u8; 32];
        thread_rng().fill_bytes(&mut msg);
        let msg = Message::from_slice(&msg).unwrap();

        let (sk, _) = s.generate_keypair(false);

        s.sign(&msg, &sk).unwrap();
    }

    #[test]
    fn sign_and_verify() {
        let mut s = Secp256k1::new().unwrap();

        let mut msg: Vec<u8> = repeat(0).take(32).collect();
        thread_rng().fill_bytes(&mut msg);
        let msg = Message::from_slice(&msg).unwrap();

        let (sk, pk) = s.generate_keypair(false);

        let sig = s.sign(&msg, &sk).unwrap();

        assert_eq!(Secp256k1::verify(&msg, &sig, &pk), Ok(()));
    }

    #[test]
    fn sign_and_verify_fail() {
        let mut s = Secp256k1::new().unwrap();

        let mut msg = [0u8; 32];
        thread_rng().fill_bytes(&mut msg);
        let msg = Message::from_slice(&msg).unwrap();

        let (sk, pk) = s.generate_keypair(false);

        let sig = s.sign(&msg, &sk).unwrap();

        let mut msg = [0u8; 32];
        thread_rng().fill_bytes(&mut msg);
        let msg = Message::from_slice(&msg).unwrap();
        assert_eq!(Secp256k1::verify(&msg, &sig, &pk), Err(IncorrectSignature));
    }

    #[test]
    fn sign_compact_with_recovery() {
        let mut s = Secp256k1::new().unwrap();

        let mut msg = [0u8; 32];
        thread_rng().fill_bytes(&mut msg);
        let msg = Message::from_slice(&msg).unwrap();

        let (sk, pk) = s.generate_keypair(false);

        let (sig, recid) = s.sign_compact(&msg, &sk).unwrap();

        assert_eq!(s.recover_compact(&msg, &sig[..], false, recid), Ok(pk));
    }

    #[bench]
    pub fn generate_compressed(bh: &mut Bencher) {
        let mut s = Secp256k1::new().unwrap();
        bh.iter( || {
          let (sk, pk) = s.generate_keypair(true);
          black_box(sk);
          black_box(pk);
        });
    }

    #[bench]
    pub fn generate_uncompressed(bh: &mut Bencher) {
        let mut s = Secp256k1::new().unwrap();
        bh.iter( || {
          let (sk, pk) = s.generate_keypair(false);
          black_box(sk);
          black_box(pk);
        });
    }
}
