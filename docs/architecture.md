# Architecture

## Project Structure
```
gametrimmer/
├── core/
│   ├── scanner.py     # Scanner abstract interface
│   ├── steam.py       # Steam implementation
│   └── storage.py     # Storage management
├── rules/
│   ├── patterns.json  # Rules in JSON
│   ├── validator.py   # Regex validation
│   └── manager.py     # Rules management
├── ui/
│   └── main.py        # GUI (tkinter)
└── tests/             # Validation tests
```

## Database
SQLite will be used for:
- Scan results caching
- Operation logging
- Paths and settings storage
- Temporary storage tracking