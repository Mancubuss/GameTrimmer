# Third-party notices

GameTrimmer itself is distributed under the MIT License — see `LICENSE`. This
file records the third-party work it builds on that carries its own notice.

---

## TikiOne Steam Cleaner

<https://github.com/jonathanlermitage/tikione-steam-cleaner>

**What GameTrimmer took from it.** The idea, and the starting list of *which*
redistributable packages are worth looking for at all — DirectX and its web
installer, the Visual C++ runtimes, PhysX, OpenAL, XNA, the .NET installers,
MSXML, Games for Windows Live, Rapture3D, BattlEye, and the shared
`_CommonRedist` folder Steam games ship them in.

The rule expressions in `rules.json` are GameTrimmer's own and mostly take a
different shape — collapsed patterns such as `^directx.*$` where TikiOne
enumerates eight literal folder names — and the set has since grown entries
TikiOne does not carry (the Uplay, Ubisoft Connect and EA app installers,
`dxsetup`, `oalinst`, `d3d11install`, AMD setup packages, and a general rule
for `KB` hotfix executables). The overlap is in the target list rather than in
the text of the rules.

That distinction is not what decides the notice, though. TikiOne Steam Cleaner
is MIT-licensed, and MIT asks for one thing in return: that its copyright and
permission notice travel with the software or substantial portions of it.
Whether a handful of regular expressions over filenames chosen by Microsoft
and NVIDIA amounts to a "substantial portion" is arguable — including the
notice costs nothing and settles the question, which is the better trade.

**License:**

```
The MIT License (MIT)

Copyright (c) 2012-2017 Jonathan Lermitage

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

---

Rust dependencies are not listed here: they are declared in `Cargo.toml` and
`Cargo.lock`, and their licenses can be produced from the lockfile with
`cargo license` or `cargo about`.
