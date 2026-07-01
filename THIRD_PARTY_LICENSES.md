# Third-Party Licenses

review-engine is statically linked with third-party Rust crates. The review-engine core itself is released under the Apache License 2.0. This file lists the licenses for third-party crates bundled with release binaries.

- **Generated for**: review-engine v0.6.2
- **Date**: 2026-06-29

> Update the commit SHA above before each release by running `git rev-parse HEAD`.

The license expressions below were generated with:

```bash
cargo tree --prefix none --edges normal --depth 1 --format "{p} {l}"
```

## Direct Dependencies

| Crate | Version | License (SPDX) |
|---|---|---|
| anyhow | 1.0.102 | MIT OR Apache-2.0 |
| async-trait | 0.1.89 | MIT OR Apache-2.0 |
| axum | 0.8.9 | MIT |
| chrono | 0.4.45 | MIT OR Apache-2.0 |
| clap | 4.6.1 | MIT OR Apache-2.0 |
| futures | 0.3.32 | MIT OR Apache-2.0 |
| hex | 0.4.3 | MIT OR Apache-2.0 |
| hmac | 0.12.1 | MIT OR Apache-2.0 |
| home | 0.5.12 | MIT OR Apache-2.0 |
| inquire | 0.7.5 | MIT |
| minijinja | 2.21.0 | Apache-2.0 |
| notify | 7.0.0 | CC0-1.0 |
| once_cell | 1.21.4 | MIT OR Apache-2.0 |
| prometheus | 0.13.4 | Apache-2.0 |
| pyo3 | 0.23.5 | MIT OR Apache-2.0 (optional `python` feature only) |
| rand | 0.8.6 | MIT OR Apache-2.0 |
| regex | 1.12.4 | MIT OR Apache-2.0 |
| reqwest | 0.12.28 | MIT OR Apache-2.0 |
| schemars | 0.8.22 | MIT |
| serde | 1.0.228 | MIT OR Apache-2.0 |
| serde_json | 1.0.150 | MIT OR Apache-2.0 |
| serde_yaml_ng | 0.10.0 | MIT OR Apache-2.0 |
| sha2 | 0.10.9 | MIT OR Apache-2.0 |
| similar | 2.7.0 | Apache-2.0 |
| thiserror | 2.0.18 | MIT OR Apache-2.0 |
| tiktoken-rs | 0.6.0 | MIT |
| tokio | 1.52.3 | MIT |
| tokio-stream | 0.1.18 | MIT |
| toml | 0.8.23 | MIT OR Apache-2.0 |
| tower-http | 0.6.11 | MIT |
| tracing | 0.1.44 | MIT |
| tracing-subscriber | 0.3.23 | MIT OR Apache-2.0 |
| uuid | 1.23.4 | Apache-2.0 OR MIT |

## Notable Transitive Dependencies

A release binary also includes additional transitive dependencies. The following crates have licenses or dual-license expressions that warrant explicit notice:

| Crate | License (SPDX) | Note |
|---|---|---|
| ring | Apache-2.0 AND ISC | Dual-licensed; **both** notices must be preserved. |
| rustls-webpki | ISC | WebPKI-derived code used by rustls. |
| matchit | MIT AND BSD-3-Clause | Dual-licensed; **both** notices must be preserved. |
| webpki-roots | CDLA-Permissive-2.0 | CA root certificates bundle. |
| unicode-ident | (MIT OR Apache-2.0) AND Unicode-3.0 | Unicode data files and software. |
| ryu | Apache-2.0 OR BSL-1.0 | Used for floating-point formatting. |
| hyper-rustls | Apache-2.0 OR ISC OR MIT | HTTPS support for reqwest. |
| rustls | Apache-2.0 OR ISC OR MIT | TLS implementation. |

## Standard License Texts

The following standard permissive licenses are used by one or more crates listed above. Their full authoritative texts are available at the SPDX links below. When distributing a binary, ensure that all required copyright and permission notices are preserved.

| License | SPDX ID | Link |
|---|---|---|
| MIT License | MIT | <https://spdx.org/licenses/MIT.html> |
| Apache License 2.0 | Apache-2.0 | <https://spdx.org/licenses/Apache-2.0.html> |
| ISC License | ISC | <https://spdx.org/licenses/ISC.html> |
| BSD 3-Clause License | BSD-3-Clause | <https://spdx.org/licenses/BSD-3-Clause.html> |
| Boost Software License 1.0 | BSL-1.0 | <https://spdx.org/licenses/BSL-1.0.html> |
| Creative Commons Zero v1.0 Universal | CC0-1.0 | <https://spdx.org/licenses/CC0-1.0.html> |

## Special License Texts

The following less-common licenses are reproduced in full for clarity.

### Community Data License Agreement Permissive 2.0 (CDLA-Permissive-2.0)

Used by: `webpki-roots`

```
Community Data License Agreement – Permissive – Version 2.0

This is the Community Data License Agreement – Permissive, Version 2.0 (the “agreement”). Data Provider(s) and Data Recipient(s) agree as follows:

1. Provision of the Data

1.1. A Data Recipient may use, modify, and share the Data made available by Data Provider(s) under this agreement if that Data Recipient follows the terms of this agreement.

1.2. This agreement does not impose any restriction on a Data Recipient’s use, modification, or sharing of any portions of the Data that are in the public domain or that may be used, modified, or shared under any other legal exception or limitation.

2. Conditions for Sharing Data

2.1. A Data Recipient may share Data, with or without modifications, so long as the Data Recipient makes available the text of this agreement with the shared Data.

3. No Restrictions on Results

3.1. This agreement does not impose any restriction or obligations with respect to the use, modification, or sharing of Results.

4. No Warranty; Limitation of Liability

4.1. All Data Recipients receive the Data subject to the following terms:

THE DATA IS PROVIDED ON AN “AS IS” BASIS, WITHOUT REPRESENTATIONS, WARRANTIES OR CONDITIONS OF ANY KIND, EITHER EXPRESS OR IMPLIED INCLUDING, WITHOUT LIMITATION, ANY WARRANTIES OR CONDITIONS OF TITLE, NON-INFRINGEMENT, MERCHANTABILITY OR FITNESS FOR A PARTICULAR PURPOSE.

NO DATA PROVIDER SHALL HAVE ANY LIABILITY FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING WITHOUT LIMITATION LOST PROFITS), HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE DATA OR RESULTS, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGES.

5. Definitions

5.1. “Data” means the material received by a Data Recipient under this agreement.

5.2. “Data Provider” means any person who is the source of Data provided under this agreement and in reliance on a Data Recipient’s agreement to its terms.

5.3. “Data Recipient” means any person who receives Data directly or indirectly from a Data Provider and agrees to the terms of this agreement.

5.4. “Results” means any outcome obtained by computational analysis of Data, including for example machine learning models and models’ insights.
```

The full authoritative text is maintained at https://cdla.dev/permissive-2-0/.

### Unicode License Agreement - Data Files and Software (Unicode-3.0)

Used by: `unicode-ident`

```
UNICODE LICENSE V3

COPYRIGHT AND PERMISSION NOTICE

Copyright © 1991-2026 Unicode, Inc.

NOTICE TO USER: Carefully read the following legal agreement. BY
DOWNLOADING, INSTALLING, COPYING OR OTHERWISE USING DATA FILES, AND/OR
SOFTWARE, YOU UNEQUIVOCALLY ACCEPT, AND AGREE TO BE BOUND BY, ALL OF THE
TERMS AND CONDITIONS OF THIS AGREEMENT. IF YOU DO NOT AGREE, DO NOT
DOWNLOAD, INSTALL, COPY, DISTRIBUTE OR USE THE DATA FILES OR SOFTWARE.

Permission is hereby granted, free of charge, to any person obtaining a
copy of data files and any associated documentation (the "Data Files") or
software and any associated documentation (the "Software") to deal in the
Data Files or Software without restriction, including without limitation
the rights to use, copy, modify, merge, publish, distribute, and/or sell
copies of the Data Files or Software, and to permit persons to whom the
Data Files or Software are furnished to do so, provided that either (a)
this copyright and permission notice appear with all copies of the Data
Files or Software, or (b) this copyright and permission notice appear in
associated Documentation.

THE DATA FILES AND SOFTWARE ARE PROVIDED "AS IS", WITHOUT WARRANTY OF ANY
KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT OF
THIRD PARTY RIGHTS.

IN NO EVENT SHALL THE COPYRIGHT HOLDER OR HOLDERS INCLUDED IN THIS NOTICE
BE LIABLE FOR ANY CLAIM, OR ANY SPECIAL INDIRECT OR CONSEQUENTIAL DAMAGES,
OR ANY DAMAGES WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS,
WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION,
ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THE DATA
FILES OR SOFTWARE.

Except as contained in this notice, the name of a copyright holder shall
not be used in advertising or otherwise to promote the sale, use or other
dealings in these Data Files or Software without prior written
authorization of the copyright holder.
```
