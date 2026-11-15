//! Concurrency-safe shared runtime file store combining per-path async
//! mutexes, cross-process directory advisory locks, and SHA-256 optimistic CAS.
