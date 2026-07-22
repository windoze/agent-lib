//! OpenAI Chat/Completions request serialization and HTTP request construction.
//!
//! Filled in by M1-3: `build_request` assembles the `POST /chat/completions`
//! URL/headers/body, expands provider-neutral messages/tools into the
//! chat/completions wire shape (design doc §4.2), injects
//! `stream_options.include_usage`, and merges provider extras. This is an empty
//! shell so the module tree compiles while the request-side work is pending.
