use soroban_sdk::contracterror;

#[contracterror]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    AlreadyMinted = 3,
    TokenNotFound = 4,
    NotOwner = 5,
    StreamRejected = 6,
    EmptyBatch = 7,
    BatchTooLarge = 8,
    ArithmeticOverflow = 9,
}
