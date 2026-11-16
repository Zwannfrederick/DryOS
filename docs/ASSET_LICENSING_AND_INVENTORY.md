# DRY OS Asset Licensing, Provenance & Complete Inventory

> **License:** `Apache-2.0 OR MIT` (Original DRY OS Authorship)  
> **Third-Party Icon Packs:** `0` (Zero third-party SVGs, zero external URLs, zero `<text>` font dependencies, zero emojis/pictographs)  
> **Total Inventory:** `118` SVG sources (`6` Brand + `21` Application + `91` System), `2` `#![no_std]` Rust POD modules, `1` JSON manifest, `4` PNG contact sheets.

---

## 1. Authorship & Licensing Policy

All files under `assets/brand/`, `assets/icons/apps/`, `assets/icons/system/`, `assets/ASSET_MANIFEST.json`, `tools/build_vector_assets.py`, `userspace/src/ui/vector_assets_sys.rs`, `userspace/src/ui/vector_assets_apps.rs`, `docs/DESIGN_SYSTEM.md`, `docs/ASSET_LICENSING_AND_INVENTORY.md`, and `docs/screenshots/brand_icons/` are original works created for DRY OS and dual-licensed under **`Apache-2.0 OR MIT`** (with the `MIT` leg preserving legal compatibility when `dry-userspace` modules are linked into GPL-2.0-only / GPL-2.0-or-later Ring-3 Linux Driver Islands per `ADR-005`).

- Embedded TrueType font subsets under `assets/fonts/` (`DrySans-*`, `DryMono-*`, generated from Noto Sans / Noto Sans Mono by [build_font_subsets.py](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/tools/build_font_subsets.py)) remain separately licensed under **SIL OFL 1.1**.
- Every SVG asset is strictly pure ASCII geometry (`<svg>`, `<defs>`, `<linearGradient>`, `<stop>`, `<g>`, `<rect>`, `<circle>`, `<line>`, `<polyline>`, `<polygon>`, `<path>`) with zero `<text>` tags and zero external references.

---

## 2. Generated Pipeline Artifacts & Contact Sheets

All generated artifacts are verified byte-for-byte deterministic via `python3 tools/build_vector_assets.py --verify`:

| Artifact Path | Role | Size / Lines | SHA-256 / Dimensions | Status |
| :--- | :--- | :---: | :--- | :--- |
| [vector_assets_sys.rs](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/userspace/src/ui/vector_assets_sys.rs) | `#![no_std]` POD table (`91` system icons, `1227` shapes) | `577` lines (`< 750`) | `446b0aaa1dc7c319f132d4022c8a4fe60f9548ea63ff92bc55461966f55dcb68` | `VERIFIED` |
| [vector_assets_apps.rs](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/userspace/src/ui/vector_assets_apps.rs) | `#![no_std]` POD table (`6` brand + `21` apps, `1712` shapes) | `661` lines (`< 750`) | `5e5944950f939fe1ab229850e3bb42a309acb2237b203c643cdcba5bfd9f52db` | `VERIFIED` |
| [ASSET_MANIFEST.json](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/assets/ASSET_MANIFEST.json) | Machine-verifiable SHA-256 asset ledger | `966` lines | `118` SVGs + `2` `.rs` modules | `VERIFIED` |
| [contact_sheet_dark.png](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/docs/screenshots/brand_icons/contact_sheet_dark.png) | Obsidian Dark visual inspection sheet | `1320x1140` PNG | Pure-Python zlib SDF render | `VERIFIED` |
| [contact_sheet_light.png](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/docs/screenshots/brand_icons/contact_sheet_light.png) | Frosted Daylight visual inspection sheet | `1320x1140` PNG | Pure-Python zlib SDF render | `VERIFIED` |
| [contact_sheet_hc.png](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/docs/screenshots/brand_icons/contact_sheet_hc.png) | High Contrast visual inspection sheet | `1320x1140` PNG | Pure-Python zlib SDF render | `VERIFIED` |
| [contact_sheet_scales.png](file:///home/zwannfrederick/Masaüstü/Sektor/Coding/dryos/docs/screenshots/brand_icons/contact_sheet_scales.png) | Multi-scale `16..256 px` optical ladder | `880x2504` PNG | Pure-Python zlib SDF render | `VERIFIED` |

---

## 3. Complete SVG Inventory (`118` Assets)

### 3.1. Brand Identity (`6` SVGs — `assets/brand/`)

| ID | ViewBox | Shapes | SHA-256 | License |
| :--- | :---: | :---: | :--- | :--- |
| `dryos-lockup-boot` | `320x80` | `260` | `397b358988a86be2cf1dde9eabef87666d4de1b332baecde4916837c1a1d9b74` | `Apache-2.0 OR MIT` |
| `dryos-symbol` | `64x64` | `186` | `774fae5bdf813edd81bb03642ef901c7c095bc4f11d0a8db2ba894e1a0e5cebc` | `Apache-2.0 OR MIT` |
| `dryos-symbol-small-16` | `16x16` | `10` | `952adea61c79d5bc130b1e65105aa44be5f6e6a04c9e520b64ffd42b9617d67d` | `Apache-2.0 OR MIT` |
| `dryos-wordmark-dark` | `240x48` | `275` | `3af39d5a91624f81aab124186779127ccc1c874583aaa24aadc98f4c0b6fc5da` | `Apache-2.0 OR MIT` |
| `dryos-wordmark-hc` | `240x48` | `275` | `efc6c3c7d1089659210ab6359125da74d03957cf887b12a0de62d6a4ef6b33e0` | `Apache-2.0 OR MIT` |
| `dryos-wordmark-light` | `240x48` | `275` | `3eb37c90377e1d3dbc2f82a27c7cca438929b8193bb1566b87f22990b6273a54` | `Apache-2.0 OR MIT` |

### 3.2. Application Icons (`21` SVGs — `assets/icons/apps/`)

| ID | ViewBox | Shapes | SHA-256 | License |
| :--- | :---: | :---: | :--- | :--- |
| `audio-settings` | `64x64` | `61` | `1c90176d9ce6aab60a1775760d37ecf791175f8f0015688a0eecbeb9f22e5556` | `Apache-2.0 OR MIT` |
| `calculator` | `64x64` | `12` | `01bf84e1d71aef7b19350872aacac15fd3f8aa6aabc45b9d256d7c8654733750` | `Apache-2.0 OR MIT` |
| `device-manager` | `64x64` | `10` | `ff824f776b1a00be844fd86981fc14e4fdef8c90615e8949c4ea9e84505a3dee` | `Apache-2.0 OR MIT` |
| `display-settings` | `64x64` | `25` | `f67e8ab1e996577ba55c2837abb74610da505142551ad6ca72409d629e72aa9d` | `Apache-2.0 OR MIT` |
| `driver-manager` | `64x64` | `14` | `054457e2a83777ca954684527ec222d12dd2af957b669491830c253db1defa09` | `Apache-2.0 OR MIT` |
| `files` | `64x64` | `45` | `193348b67404e75665627d8a19f50a535fb78d1f3f4b4dbcf11d4f3c8f7d3c35` | `Apache-2.0 OR MIT` |
| `help-diagnostics` | `64x64` | `7` | `e4ac9830ac1609f1388e7d903cf84b80691d7f813b9a361cbf7fbbf1ec461c85` | `Apache-2.0 OR MIT` |
| `launcher` | `64x64` | `9` | `64cc02e2d4a7948e7d0b0e6d2643b0030d9258dc7edb44b92e21e658eaeeb7ff` | `Apache-2.0 OR MIT` |
| `life-station` | `64x64` | `13` | `e497f7f80b639130cd9af632da92d10ce79c0043384cd6889a40af7a852bbd82` | `Apache-2.0 OR MIT` |
| `network-manager` | `64x64` | `20` | `2a3427127da64560550980830a36abffa7f42ac37548bd598ba6b755202fb94c` | `Apache-2.0 OR MIT` |
| `notes` | `64x64` | `9` | `fb4c62ad0dd0613a1c0870ae615bd39a99b92a549b4bdff15aef3b5e38604c57` | `Apache-2.0 OR MIT` |
| `power-manager` | `64x64` | `23` | `b3192360f02a13f3dad026e1ef30932e5ab6a624f930f44107e9eaef41ffa5c5` | `Apache-2.0 OR MIT` |
| `security-vault` | `64x64` | `67` | `64d1b426a6d08f73e430f3525245ce587651d40fb7d52e165c818e83fa5e7424` | `Apache-2.0 OR MIT` |
| `settings` | `64x64` | `13` | `1120245c108e791b2af34a66530afee38ce36d59ea88eebe82d2c31fdb38e755` | `Apache-2.0 OR MIT` |
| `software-store` | `64x64` | `23` | `b996008d8f937efc1aca36fc90385ce7db102ee212ce32cfdd75e365b38f4a97` | `Apache-2.0 OR MIT` |
| `storage-manager` | `64x64` | `16` | `f8ed0a63a78731ffb8eea180e8784492b5f7dc1f69700c9d6095001f6578394f` | `Apache-2.0 OR MIT` |
| `system-info` | `64x64` | `6` | `890ee1c9bbb9e56028e37f44a880cfa7762c583719c43bbb2e3094c2962fbb5d` | `Apache-2.0 OR MIT` |
| `system-monitor` | `64x64` | `11` | `8f58fdcbf7d957d203a267a68beafc872fd359ff24bd75c3e01579117e668377` | `Apache-2.0 OR MIT` |
| `terminal` | `64x64` | `10` | `eb018425de15d2cd7237a6b3ff1fe7550a6f99d5ef00da79c8079caa36b02766` | `Apache-2.0 OR MIT` |
| `text-editor` | `64x64` | `9` | `78d9f83a5e224224f1be736a008fcde5917c1fa22ee9d4a04f71b0683ec0b8fd` | `Apache-2.0 OR MIT` |
| `update-manager` | `64x64` | `28` | `af6caaee86a2a41cacf8eee7d9d69307cea3c3f18660daa4d590506e3b66d718` | `Apache-2.0 OR MIT` |

### 3.3. System Iconography (`91` SVGs — `assets/icons/system/`, `24x24`)

| ID | Shapes | SHA-256 | ID | Shapes | SHA-256 |
| :--- | :---: | :--- | :--- | :---: | :--- |
| `accessibility` | `5` | `4d887485d5b4dc685afabac6a96513df022e1d98ec3952b081689a7a4cb45278` | `notif-unread` | `20` | `d2286d755a72709cc59440e07a0ab859293dd491734e320ae26bea91d7a1586e` |
| `add` | `2` | `8ca8b1a61275cd5c9768c870d6ea031dadb5f6c20085920f2100e00037562477` | `offline` | `2` | `1882b67ba05ee5f069294b1b49cb3305d85e3d5cdcd9a2181f82a22bde26a804` |
| `audio-out` | `18` | `40d8bca7f4e8fb14ad804ae077015d862dfec3d039dcc9d6f76176f1950aecf2` | `online` | `2` | `94ac9d861c3b1bea793f0141af12161a57d445043990cb66ad3dc8e21a331f1a` |
| `back` | `3` | `210df3e6a206f40aa2e3485ddc11f18432204e73d5723f1ec76320d89e13135e` | `open` | `19` | `e8861fe2880e38082b45f9bd7aa9606d9704b9821b1e06f248733ec99019dd1e` |
| `battery` | `18` | `97a1342cd62272720642dbd0ad1ba8af96214804da2ae5bd95828780c8aa5fcb` | `options` | `3` | `b2d7523b33afb16899b3f19920f30e0a0e27f3f505516ca3e29abb7aa7bd2829` |
| `close` | `2` | `a88e57a6f0e4e8aba4fb37493e369ba09cc0b774d6d4118501e9b48a219bbbe8` | `partition` | `3` | `9473687a2434449c9c573809335d9fa77f2cfed61a85f8f57d29e1a498485ec2` |
| `collapse` | `6` | `22d6751a97dc42551ee81fc1b391e9abaff3fdce743b7c924d23920f94caf8d2` | `paste` | `19` | `6132d83a6db280190302913c4f24473673f4109a01e2bbf2f3a5e9c9518b2301` |
| `connected` | `3` | `ad778803e372031a2aaf25e4851cf7eaa9460397e27de0b373c47df2a9cdcc30` | `permission-cap` | `7` | `697f4619afaeb589bccc81fb02437ff45dc1f6826cecbe555d5adb0283cc7f8d` |
| `copy` | `20` | `53983a1357a9ed0ff190df30381fd25d50b001e1b6b7bc27d651d39895f3df89` | `power` | `20` | `d1519a4114bb6f9d5bec15ad886f2b91f80c99315fd1b20a6729c323b425fcc4` |
| `cpu` | `25` | `bf64feecc01eda02f15a0522939e5e46d2d2c85b7ebf439259c6feeb5dbb4aef` | `progress-ring` | `18` | `c99d55502eb4fc3201091b9d7fc1df673d3f5d71b596397ddbc1e552037a554e` |
| `cut` | `4` | `23ed7be16d73f169a610e80fbaa0ebfef4edc516597306c64122c48656e1a2d0` | `quick-settings` | `6` | `b8c33fbdff2a5849c55d9e5d175307c61db62385cdcae9974755407cc6971e8a` |
| `device-ok` | `18` | `1362648e9f6b682606ae306bf34b4cdb661bd87ae4265542a8f289af60c5d9c7` | `ram` | `20` | `4c21f3810ac4dcaa69fbb801607170f518a14b0fb7b74e306e81aa872794be5f` |
| `disconnected` | `3` | `c0d9e32279760bc4f767bd0fb02b79c8f90eb57ef7d7cbc38db9a985c13ea43b` | `redo` | `17` | `ef635a29daf18835351c072dbb94a833d5ccf67e572ace63fdf582d691f4bf01` |
| `disk` | `3` | `95e5fea7bd91ea6a5bef2c5b25204f685e4123627b94d12444550acacf8e2523` | `refresh` | `24` | `0d6d7333176170030a5941ac6f41c1ba170427b33899ab17d698fbe4386dc4ff` |
| `display` | `18` | `6ffa9406e9cdaca25b4640a77d19b6a215b2c14cc2074cc5765ccdb882a44b60` | `remove` | `1` | `0a1fe1b151ea34c46ba1137248613b91f7bc8f89797092be8dae19e0a83e2ea7` |
| `dock-dot` | `2` | `bc4e09cd739187b7277d60bb55432d972b9ccb0c23fc07f6c66ce38db5b32dc7` | `save` | `33` | `3425f225a27286fa40b6840c8facc8de25f3c7b1ae1403edad55699a3e431011` |
| `down` | `3` | `54917aa44c2d9a77cc321cc63b67b58996ae6805ae5521d0faa581635316566b` | `search` | `2` | `561d09b229490f6ac23a2c1d90ccd1c751e3d53a63d287c90e3957889f099f5d` |
| `download` | `6` | `9b9ad08a4b87df230f5909a956fd07e86c0fcbee3f3539e07e0b628f62df980a` | `sort` | `3` | `8b60b7d37b1015dcbf04c727e15f4445b1b5a128e44023722d5479e1cf5ff291` |
| `drive-nvme` | `19` | `37f7c89e038a316db51196df7e57175b85abb02101af403564a6d3de65f14078` | `spinner-arc` | `8` | `6e9062534fde728edabffe68111858fa17aaf1bb61a52fda46d49daeaa19311d` |
| `drive-usb` | `33` | `837a29d4c22780073d3a1b24e99d19ec8fb766effcfbd87a47e9fea6e2c633f5` | `status-failure` | `3` | `e1478cd853d6fc1206b5f5d85adb82200bac2fbab3320af454495e177861ca47` |
| `driver-island` | `25` | `2a4ee6b8eb6bd1c60c217fe580ecfd186cf31b552cd39ed0f74c896095653133` | `status-info` | `3` | `134cf658a24bf6d1f48db1cbe5649b351fb29e2a35fc42532c28d2c7f238bf3d` |
| `edit` | `3` | `032e7e4ed2de34c792003943911c3316fa0c93acf3ac598211880d3d6b056474` | `status-success` | `3` | `f4a60dfbb8895328db35b5d272065a678ed0817d5ce244e1f1cd4ba81465ed59` |
| `expand` | `6` | `b71c93b28180e2d6f7771b42dd6448c9d0d0e292fd49e4b1aad6f346674aeb30` | `status-warning` | `5` | `65d5bd7336a429fa1bb99f69a655938626b77f305dbd1a2a689483884a4bffcc` |
| `file-archive` | `18` | `ce0cead8c4844067ce9bbb34c8f710880b1d8ef543b48ffa425a16408fc5df61` | `storage-ctrl` | `21` | `859760cc1c9651b1a8de876022fc53aadf8eaec546ef7dd8eb22fb2e4b97db3f` |
| `file-audio` | `19` | `18ce31bfdbe30697505a2749a0e552e5256e1b71c3c800f88c95061756ed5b7a` | `svc-crashed` | `6` | `f9f6255211fe84992acaae88c8e17bcb6f74590c39e84734d846bd6f34968c87` |
| `file-code` | `21` | `2335efb42280816f5e7bb3cc35c7b07f24f50d36d38d9958e4629b043484b5fc` | `svc-running` | `17` | `b00f11d6602a9ad5f9a02e8429bb0962898a1f15ca01622395bc63de95d69f96` |
| `file-doc` | `19` | `333fa4b69e5e09ac2e0502436126c658e8612c3be9167dbd21956ccb08dafa4a` | `svc-stopped` | `2` | `3e8869899fa8063c3c99b8d33c6994da853159c0b6c3029e7822e9a73985641b` |
| `file-exec` | `19` | `ff45f4978318e4e3cdcff9c00e91e8266197bfe5f183e905607abd37494d67ae` | `sync` | `22` | `35c73095f2eb3a3ba69329650a1b52e2bd263fd15bbd78c07c4a258375012c9e` |
| `file-image` | `21` | `67cd2dadcfd0df144216926ea262a44d0ba4541295b9cec4f6cce0116715d0dc` | `tile-float` | `33` | `98322608743948a1679e065855e3d61d45af6763b9863b05066500a02e56bca5` |
| `file-video` | `29` | `2112ca542e6d6f68e11a1c27f4c7983968189e2e811614813f259314f26dbf15` | `tile-h` | `48` | `b8f4826bc7cf00f3589d5d91d65464237faba1a9685686613f70fd6bb5c3af17` |
| `filter` | `7` | `6dfd8563ca7851b7f290165a82ae8c8432f99284d892662337541ea8dd12ee40` | `tile-stack` | `18` | `de48058ccfec5485a8e7dba7cbfa63628836563dd4f9043843f7373e22c1e3aa` |
| `folder` | `17` | `71855680b364dbf664eab756cd3195e598e6db9667d8a7e952b22159f93536ae` | `touchpad` | `18` | `2774e1e1b1dd23fdfa469d9657566006dd0651ace823d094d1a45d4b5331cf56` |
| `folder-open` | `9` | `6033d21dd6b15ade02f91c5287f47fc80242ae1d0d3e7ccc5217f0ff0164c7fe` | `trash` | `20` | `97bdca6cfe4de004dd4d21026b1d11cedf67af5eb1b4d3eb90a1146271562728` |
| `forward` | `3` | `fedda04f61298e1147705ece23b88c8f19f39c5e98a2d83cf7f22c8c0d41b323` | `unavailable` | `2` | `104662349db946250bea009631f073d93fd23a16401da68baf5abb041ca9b16f` |
| `gpu` | `19` | `ab69ae449ff5ebeeb8b63dea8a25f22ba2f67e6a41a8531a0db879b483ae2375` | `undo` | `17` | `644c3994f569862ddf9dcbbbae110f7c35d0137afab113d49885397fd7f17e0f` |
| `home` | `6` | `d4dc28fb5f6a07a460e436ec31126d088598a8fc7a5a0eec5828bf1361be7f1e` | `unlocked` | `29` | `64ddf8544d63a6e96a4320a0c11a74a5c3c0036f47eebcf0d7027de32d5eeae2` |
| `keyboard` | `18` | `bfe59a6f7d90ab8ac900da0238096ea26ed199ef86b08e543f759255dfe6f17e` | `up` | `3` | `ad5e7fc496b2b938db67f3385d03cf9c5eb9a80555e1215df7484f68f4916bf2` |
| `locked` | `32` | `d27220b6c01803215e53723b124fe2cedad3239b700ece3812d9a5be64d99286` | `updating` | `4` | `3bba8740e356603e6ba8c32c58f78c3344d7bcab3365267e8a134fb5fc8f13b5` |
| `maximize` | `17` | `534f1a39478487233e85104631c6012f0f9fa416e36d6bcde16d77fe2e2135a0` | `upload` | `6` | `0e5974b39d9980907089ee61d74129d34f1f94238d788bad1c2caedc16be1d7e` |
| `menu` | `3` | `896f76d6f6fd5ed861576b7a237eba2fd708351d61aef7f99da878cca048442e` | `usb-hid` | `8` | `9e2aedd0226cbaa36f9631865fc55890d54ca2ed2f6bc3898cfbcfd06fc4fd89` |
| `minimize` | `1` | `89f9ec36a9752403be767d8eda84cdbdc9bb56cd8891e6038634025d8cdc5812` | `usb-port` | `17` | `deaf76bb3871b50dad6502faa3947b656e1eb68cc4be786f3a98c575a2735daf` |
| `mouse` | `17` | `9a86cd4c99e8c0fc991a241c950758f9b2a81a83ea950536a003bd2a14f2bae2` | `win-close` | `2` | `295fc017e6d2e17398740a9c735b9d4c3b35b37c99f278a788527a9184a520e2` |
| `network-eth` | `20` | `ef16dbd05e46adb7a03953c57c4dcd22b59e98cc83b3adc8c9acf6f536d52bf1` | `win-max` | `16` | `4c6f6a84f0412bcb64fcf985fdefebf6de3efafb821fee3d32d411a5fc09099d` |
| `network-wifi` | `13` | `ffa696b7ba891c998ede5095e891c26983060e7d592615a87202704680d712e7` | `win-min` | `1` | `2aace0a045bedeb15b07fec1b59374b808e9d7b3265857e5ebeaef544782a4a8` |
| `notif-bell` | `19` | `718008be780942e196d811571004c2af095ea1a5b3caaebde69c284269573f90` | `win-restore` | `20` | `55328f72fec50d254697b8ed98188a21c4b4869bb0e4e25aede6ecc77b7eaa22` |
| | | | `workspace-grid` | `64` | `b72e7ff2b993ed7baf8fd74223944d7926ca85e0cd56d16ed505b7806a228b00` |

---

## 4. Verification & Reproducibility Gate

Verified with `python3 tools/build_vector_assets.py --verify` (exit code `0`):
- `[VECTOR-ASSETS] OK | brand=6 apps=21 system=91 total_svg=118 | sys_rs_lines=577 apps_rs_lines=661 | sys_shapes=1227 app_brand_shapes=1712`
- Checks enforced: `6` brand + `21` app + `>= 80` (`91`) system SVGs, printable ASCII only (`0` emojis/pictographs), zero `<text>`/`<tspan>` tags, zero external URLs, Rust modules `< 750` lines, compiler `< 700` lines (`686`), and byte-for-byte deterministic match across `.rs` tables, `ASSET_MANIFEST.json`, and all 4 PNG contact sheets.
