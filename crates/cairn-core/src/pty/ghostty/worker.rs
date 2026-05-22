//! Session worker thread: bootstraps the current-thread tokio runtime,
//! runs the PTY reader task and the command dispatcher on a `LocalSet`.

// (Implementation lands in later tasks.)
