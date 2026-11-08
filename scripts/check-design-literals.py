#!/usr/bin/env python3
"""Guard control styling, not general arithmetic or wallpaper geometry."""
import pathlib
import re
import sys

root = pathlib.Path(__file__).resolve().parent.parent
paths = [pathlib.Path(p) for p in sys.argv[1:]] or sorted((root / "src").rglob("*.rs"))
failed = False
for path in paths:
    if "/src/design/" in path.as_posix() or path.name == "preview.rs":
        continue
    source = path.read_text()
    # Test modules live at the end of these Rust files. Strip comments and
    # strings before looking for calls, so examples and labels are not code.
    source = source.split("#[cfg(test)]\nmod tests", 1)[0]
    source = re.sub(r'//[^\n]*|/\*.*?\*/|r#+".*?"#+|"(?:\\.|[^"\\])*"',
                    lambda m: "\n" * m[0].count("\n"), source, flags=re.S)
    for match in re.finditer(r'\.(?:size|padding|spacing|line_height)\s*\(|(?:Color::(?:from_\w+|rgba?|rgb)|Metrics::new)\s*\(|\bPadding\s*\{', source):
        start = match.end()
        opening = source[start - 1]
        stack = [")" if opening == "(" else "}"]
        end = start
        while end < len(source) and stack:
            c = source[end]
            if c in "({[":
                stack.append({"(": ")", "{": "}", "[": "]"}[c])
            elif c == stack[-1]:
                stack.pop()
            end += 1
        args = source[start:end - 1]
        # Indexes and zero insets are structural, not new visual choices.
        args = re.sub(r'\[\s*\d+\s*\]', '', args)
        args = re.sub(r'/\s*2(?:\.0)?\b', '', args)
        numbers = re.findall(r'(?<![\w:])\d+(?:\.\d+)?(?:f32|f64|u8)?\b', args)
        if any(float(re.sub(r'(?:f32|f64|u8)$', '', n)) != 0 for n in numbers):
            line = source[:match.start()].count("\n") + 1
            print(f"{path}:{line}: design literal outside src/design; use a token", file=sys.stderr)
            failed = True
if failed:
    sys.exit(1)
print("Design literal check passed")
