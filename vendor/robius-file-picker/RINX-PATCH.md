Source: project-robius/robius cca3cc3dc99d8fcb6c69a127cd28d47ba8f3e3eb, crates/file-picker (MIT).

Rinx adds a guarded Android save: revalidate account/instance authority on the save worker immediately before opening the selected content URI for writing. Ordinary picker calls retain their behavior. Cancel/error/completion removes the guard. Workspace dependencies are resolved to the original versions and pinned source.

On macOS, `run_dialog_then` starts the callback's worker on the calling thread and runs only the dialog in the main thread's GCD block, which hands its result to that worker. Upstream starts the worker inside the GCD block; under a host that runs the app's code on a thread of its own (Makepad's JIT runner), such a worker is outside the app's context and its actions and popups are lost.
