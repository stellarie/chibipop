# Test fixtures

`japanese_bgra.bin` — a 400×120 tightly packed 32bpp BGRA buffer, alpha forced
to 0xFF, containing the text 昨日は rendered in Yu Gothic UI at 36pt on white.

Stored raw rather than as a PNG so the OCR integration test needs no image
decoder: this is byte-for-byte the format `capture.rs` produces, so the test
exercises exactly the production path. Regenerate with the script in
the m2-ocr-tier plan, Task 7 (working notes, not published).
