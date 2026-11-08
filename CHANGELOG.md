# Changelog

## [0.22.0](https://github.com/alkanna/quarry/compare/v0.21.0...v0.22.0) (2026-11-08)


### Features

* new Workbench dark and Paper light themes with an IDE-style layout ([#130](https://github.com/alkanna/quarry/issues/130)) ([ad26de4](https://github.com/alkanna/quarry/commit/ad26de4e2ca2b1894d77b977e78f0c2effc159ae))

## [0.21.0](https://github.com/alkanna/quarry/compare/v0.20.0...v0.21.0) (2026-11-08)


### Features

* encrypt the Query value suggestion cache at rest ([#128](https://github.com/alkanna/quarry/issues/128)) ([22dc0e3](https://github.com/alkanna/quarry/commit/22dc0e3421dafc7cd63fb104e2a7ab18ca7a92a3))

## [0.20.0](https://github.com/alkanna/quarry/compare/v0.19.0...v0.20.0) (2026-11-08)


### Features

* Query box suggests values from the whole collection, with type-ahead ([#127](https://github.com/alkanna/quarry/issues/127)) ([7f4f892](https://github.com/alkanna/quarry/commit/7f4f89258fbb81acc331ecd838e7d6edcc67043b))
* Schema analysis can read the last 1,000, 10,000 or 100,000 documents ([#125](https://github.com/alkanna/quarry/issues/125)) ([df7bf14](https://github.com/alkanna/quarry/commit/df7bf14e98d66c214886e614c8e9c5754202ac6d))


### Bug Fixes

* Aggregate stage previews show their sample when counting times out ([#123](https://github.com/alkanna/quarry/issues/123)) ([4b32ff8](https://github.com/alkanna/quarry/commit/4b32ff8abb597f5e27ec1775044d72d22f0f6fe7))
* Query box accepts dotted keys without quotes ([#126](https://github.com/alkanna/quarry/issues/126)) ([3651cdd](https://github.com/alkanna/quarry/commit/3651cddb5c4c4648baa8ab8ae5265a4ee2bd89e6))

## [0.19.0](https://github.com/alkanna/quarry/compare/v0.18.2...v0.19.0) (2026-11-08)


### Features

* memory and CPU readouts in the status bar, steadier frame rate ([#121](https://github.com/alkanna/quarry/issues/121)) ([89d0b9f](https://github.com/alkanna/quarry/commit/89d0b9f61bac50d294bda7d04571865e35b08dac))


### Performance Improvements

* keep Shell result documents once, as BSON bytes ([#122](https://github.com/alkanna/quarry/issues/122)) ([7515b16](https://github.com/alkanna/quarry/commit/7515b16478217c0088516a0983d75512efd9a589))
* read related-document ids from BSON bytes instead of decoding the page ([#119](https://github.com/alkanna/quarry/issues/119)) ([fedbaab](https://github.com/alkanna/quarry/commit/fedbaabfccfc686f85bf1c34bd5ba72e9444d434))

## [0.18.2](https://github.com/alkanna/quarry/compare/v0.18.1...v0.18.2) (2026-11-08)


### Bug Fixes

* never decode a whole page at once, so its memory is not kept ([#115](https://github.com/alkanna/quarry/issues/115)) ([8ff7385](https://github.com/alkanna/quarry/commit/8ff7385607894774cdb2118738d31103316a911b))


### Performance Improvements

* keep Aggregate results and Watch events as BSON bytes ([#118](https://github.com/alkanna/quarry/issues/118)) ([081f8ed](https://github.com/alkanna/quarry/commit/081f8edddd2c81eeb64d5b2636a31f71081e6318))

## [0.18.1](https://github.com/alkanna/quarry/compare/v0.18.0...v0.18.1) (2026-11-08)


### Performance Improvements

* keep the Documents view's pages as BSON bytes to cut memory ([#113](https://github.com/alkanna/quarry/issues/113)) ([46a9370](https://github.com/alkanna/quarry/commit/46a9370075aed0630e742b658cb0de5a53c62392))

## [0.18.0](https://github.com/alkanna/quarry/compare/v0.17.1...v0.18.0) (2026-11-08)


### Features

* indexes as table rows with a full New index dialog ([#110](https://github.com/alkanna/quarry/issues/110)) ([ee2fc02](https://github.com/alkanna/quarry/commit/ee2fc02f534fe94a5d0523ba1012d2f4e772f741))
* Server window with health, replica set and running operations ([#111](https://github.com/alkanna/quarry/issues/111)) ([be39a7d](https://github.com/alkanna/quarry/commit/be39a7d7c2fb696d7b30631c0ecb1c82978c841d))
* Watch view that streams a collection's changes as they happen ([#112](https://github.com/alkanna/quarry/issues/112)) ([b2b15e8](https://github.com/alkanna/quarry/commit/b2b15e869fdc743a600330d4e18ad409df6afa0e))


### Bug Fixes

* index usage stats refused by the server no longer fail the Indexes view ([#104](https://github.com/alkanna/quarry/issues/104)) ([3f426c0](https://github.com/alkanna/quarry/commit/3f426c0e925870b84356f0e9435bcac60383ef56))
* keep bulk renames and drafts from touching data they should not ([#109](https://github.com/alkanna/quarry/issues/109)) ([56f4888](https://github.com/alkanna/quarry/commit/56f48887d7918ecd40c9ac1883db02d114041dab))
* Users and roles fits on screen and works for users who may not list everyone ([#107](https://github.com/alkanna/quarry/issues/107)) ([0b89f64](https://github.com/alkanna/quarry/commit/0b89f6431db38e331d997ef831a2b53bb2fdf81a))
* what's new after an update covers every release since the last run ([#106](https://github.com/alkanna/quarry/issues/106)) ([e3ee104](https://github.com/alkanna/quarry/commit/e3ee10441ab407c5baf6ce9537f3efa0f46a4aab))

## [0.17.1](https://github.com/alkanna/quarry/compare/v0.17.0...v0.17.1) (2026-11-07)


### Bug Fixes

* open documents without an _id keep their own field fold state ([#102](https://github.com/alkanna/quarry/issues/102)) ([b040a22](https://github.com/alkanna/quarry/commit/b040a22b7e2fda3a706c517ccab6bf222de6ec62))

## [0.17.0](https://github.com/alkanna/quarry/compare/v0.16.0...v0.17.0) (2026-11-07)


### Features

* query value shortcuts, field value suggestions, query help and page jumping ([#100](https://github.com/alkanna/quarry/issues/100)) ([50eb9b8](https://github.com/alkanna/quarry/commit/50eb9b81bc79a6556eeb20682fb28a1fa710ffdb))

## [0.16.0](https://github.com/alkanna/quarry/compare/v0.15.0...v0.16.0) (2026-11-07)


### Features

* paged compare results in the shared table, rows read where each view keeps them ([#98](https://github.com/alkanna/quarry/issues/98)) ([77441fd](https://github.com/alkanna/quarry/commit/77441fd1a9b8b8d1cf4c48ecd0abad65b4d5466d))

## [0.15.0](https://github.com/alkanna/quarry/compare/v0.14.0...v0.15.0) (2026-11-07)


### Features

* shell editor intelligence from the tsrs language server, IDE-style suggestion list and column stats for dates ([#96](https://github.com/alkanna/quarry/issues/96)) ([7a5e977](https://github.com/alkanna/quarry/commit/7a5e9777d87cf4c7b35075f44a57988b38bc8fb1))

## [0.14.0](https://github.com/alkanna/quarry/compare/v0.13.0...v0.14.0) (2026-11-07)


### Features

* column header popover with sort, alignment, widths, data actions and shell writes ([#94](https://github.com/alkanna/quarry/issues/94)) ([22d5edd](https://github.com/alkanna/quarry/commit/22d5edd048f6209537d09226c2a91f6dcc3433d3))

## [0.13.0](https://github.com/alkanna/quarry/compare/v0.12.0...v0.13.0) (2026-11-07)


### Features

* attached tab strip with overflow list, centered text and hand cursors ([#92](https://github.com/alkanna/quarry/issues/92)) ([99d5c42](https://github.com/alkanna/quarry/commit/99d5c42441a1f9d80b2235c8af4a4926c5178ea8))

## [0.12.0](https://github.com/alkanna/quarry/compare/v0.11.2...v0.12.0) (2026-11-07)


### Features

* consistent type scale, buttons and icons, and a steadier query log ([#89](https://github.com/alkanna/quarry/issues/89)) ([c60d2aa](https://github.com/alkanna/quarry/commit/c60d2aabbaad43e7de28327d5bcf06f1c7977d10))
* tag connections and polish the connection picker and dialog ([#86](https://github.com/alkanna/quarry/issues/86)) ([fefcc65](https://github.com/alkanna/quarry/commit/fefcc65a64e9d22dec02de0723e778af486d902f))


### Bug Fixes

* make Expand recursively, Expand all and Collapse all work again ([#85](https://github.com/alkanna/quarry/issues/85)) ([90dd533](https://github.com/alkanna/quarry/commit/90dd533a30e901734fa0be3d1438ef981c7be9dd))

## [0.11.2](https://github.com/alkanna/quarry/compare/v0.11.1...v0.11.2) (2026-11-06)


### Bug Fixes

* open document rows with their nested fields collapsed ([#83](https://github.com/alkanna/quarry/issues/83)) ([cda7e26](https://github.com/alkanna/quarry/commit/cda7e26abca6facdef6813c3c83ba1c1c5e85c44))

## [0.11.1](https://github.com/alkanna/quarry/compare/v0.11.0...v0.11.1) (2026-11-06)


### Bug Fixes

* ask before running shell code that uses $merge ([#77](https://github.com/alkanna/quarry/issues/77)) ([d6036a3](https://github.com/alkanna/quarry/commit/d6036a3cf52b2ed4933efa3c91ff887ac6bcc22b))
* count and undo documents inserted before a failed import batch ([#66](https://github.com/alkanna/quarry/issues/66)) ([dddd92a](https://github.com/alkanna/quarry/commit/dddd92a3234010f738ce52a94a1f90f21280923e))
* delete a document with unsaved edits against its stored version ([#63](https://github.com/alkanna/quarry/issues/63)) ([3284682](https://github.com/alkanna/quarry/commit/3284682dfb331794f2fc7de456554a6fd691dd7e))
* drain shell stderr while it runs and let stop cancel bun install ([#70](https://github.com/alkanna/quarry/issues/70)) ([50e4cba](https://github.com/alkanna/quarry/commit/50e4cba628cbe6a1efd42cfeb5f6db474a7ca9de))
* drop the compare result when the target changes ([#60](https://github.com/alkanna/quarry/issues/60)) ([0c8d638](https://github.com/alkanna/quarry/commit/0c8d63809e16091008d9b4b8b1650727dd2c0638))
* handle accented header words in csv camel case ([#58](https://github.com/alkanna/quarry/issues/58)) ([6700ee7](https://github.com/alkanna/quarry/commit/6700ee7f990d26713f023a5fe2f864f2f64c7175))
* keep each tab's documents when a reference opens a new tab ([#59](https://github.com/alkanna/quarry/issues/59)) ([3408208](https://github.com/alkanna/quarry/commit/340820877fb93418da0996dca391929c08de2cd2))
* keep the implicit auth database when restoring ([#65](https://github.com/alkanna/quarry/issues/65)) ([7e77ff6](https://github.com/alkanna/quarry/commit/7e77ff6074ee957b141788b3f648ee852219674b))
* keep the offline demo away from the keychain ([#67](https://github.com/alkanna/quarry/issues/67)) ([586d20a](https://github.com/alkanna/quarry/commit/586d20ad700b182fc8d1e8efb2f61f272b85b048))
* keep the other connections when one entry fails to parse ([#82](https://github.com/alkanna/quarry/issues/82)) ([21c61aa](https://github.com/alkanna/quarry/commit/21c61aa61f9368a91197adaf072f5652f9706866))
* keep the password out of mongodump and mongorestore arguments ([#62](https://github.com/alkanna/quarry/issues/62)) ([086b166](https://github.com/alkanna/quarry/commit/086b166136289e5d330da83940b85e8b7900c6fb))
* lock connections again when always allow writes is turned off ([#73](https://github.com/alkanna/quarry/issues/73)) ([7c11756](https://github.com/alkanna/quarry/commit/7c11756b7ada4d504398f42fa68bd4c4fbfb8ac4))
* make the toast's undo revert the save it describes ([#79](https://github.com/alkanna/quarry/issues/79)) ([fafae74](https://github.com/alkanna/quarry/commit/fafae7476474946e6525d1900e0effede864ca67))
* reject path-like and pinned shell package specs ([#76](https://github.com/alkanna/quarry/issues/76)) ([0b11f1c](https://github.com/alkanna/quarry/commit/0b11f1c3f7f5a5761c55b3cee2b6722f6c2cf436))
* reload the tab a write belongs to instead of the current one ([#74](https://github.com/alkanna/quarry/issues/74)) ([c5a7876](https://github.com/alkanna/quarry/commit/c5a787688964bc876c2b8c4f150224fb88b5217f))
* restart the updated binary on linux instead of the old copy ([#61](https://github.com/alkanna/quarry/issues/61)) ([2787227](https://github.com/alkanna/quarry/commit/2787227a8903ecd9896520d5a0b2df5490243835))
* save the last connection with the rest of the session ([#78](https://github.com/alkanna/quarry/issues/78)) ([fcdd32a](https://github.com/alkanna/quarry/commit/fcdd32af036b26b11df200c6f460008c728f04b8))
* **shell:** close read-only guard gaps for aggregation stages, admin commands and client writes ([#64](https://github.com/alkanna/quarry/issues/64)) ([6ff9168](https://github.com/alkanna/quarry/commit/6ff9168bc3da72cd607471f53c03a790011aa143))
* **shell:** reach collections named after reserved words through db.collection ([#71](https://github.com/alkanna/quarry/issues/71)) ([e82354e](https://github.com/alkanna/quarry/commit/e82354ecf9a7ed0f0728ee6533ee0e620538277f))
* show the shell or import row in the side panel, not an aggregate one ([#81](https://github.com/alkanna/quarry/issues/81)) ([d4b8fce](https://github.com/alkanna/quarry/commit/d4b8fce9a264b2d6ac7110c0077e4f65f4d4959a))
* stop field renames from overwriting a sibling ([#68](https://github.com/alkanna/quarry/issues/68)) ([8003128](https://github.com/alkanna/quarry/commit/8003128935c08acf37dc4f717c8d2610ea448608))
* suggest field names inside shell function bodies ([#75](https://github.com/alkanna/quarry/issues/75)) ([3a5f514](https://github.com/alkanna/quarry/commit/3a5f514aa79b79f0c1180a6ebe58fdd03f0dae16))
* verify the update archive against a published sha-256 checksum ([#69](https://github.com/alkanna/quarry/issues/69)) ([0520b84](https://github.com/alkanna/quarry/commit/0520b8471179f95390105018dd762ff05dc3329c))
* write settings files atomically ([#80](https://github.com/alkanna/quarry/issues/80)) ([94e4bcf](https://github.com/alkanna/quarry/commit/94e4bcf3f746921a24970de16a94663c1a3cb465))

## [0.11.0](https://github.com/alkanna/quarry/compare/v0.10.2...v0.11.0) (2026-11-06)


### Features

* column manager with drag and drop, pin icons, filter and path suggestions ([#54](https://github.com/alkanna/quarry/issues/54)) ([ac53a8b](https://github.com/alkanna/quarry/commit/ac53a8b0a2cbc9658ff12db6af1c0ba879dcc998))
* highlight toggles show their state in field and column menus ([#53](https://github.com/alkanna/quarry/issues/53)) ([dcc56ed](https://github.com/alkanna/quarry/commit/dcc56edfcec785456cc4f58a37fcf72a5efff0bb))
* native macOS menu bar with About panel and shortcuts ([#56](https://github.com/alkanna/quarry/issues/56)) ([7dcf1e2](https://github.com/alkanna/quarry/commit/7dcf1e28ff962631b7c70e07ca2ef1c5567dcf85))
* open several documents inline at once ([#52](https://github.com/alkanna/quarry/issues/52)) ([72b68de](https://github.com/alkanna/quarry/commit/72b68def5dd7abeeee3b94f3dbb5417a28ef3855))
* select several documents for bulk actions ([#51](https://github.com/alkanna/quarry/issues/51)) ([2994ae8](https://github.com/alkanna/quarry/commit/2994ae8bafef13f9eed27e95f0171426afd371a2))


### Bug Fixes

* aligned values, row hover and full-width cards in the document tree ([#50](https://github.com/alkanna/quarry/issues/50)) ([36808a7](https://github.com/alkanna/quarry/commit/36808a7d1d122b6caa12bac9fc78b2a1ca254889))
* single border around the sidebar and inspector ([#47](https://github.com/alkanna/quarry/issues/47)) ([6149bb6](https://github.com/alkanna/quarry/commit/6149bb61df8b472c268a7fc6583f18fa54ebea91))
* wrap the view bar and document header instead of overlapping ([#55](https://github.com/alkanna/quarry/issues/55)) ([2278812](https://github.com/alkanna/quarry/commit/227881229bed1b14f0e6b981aa2ea77ba0987199))


### Performance Improvements

* spinners stop redrawing behind other windows ([#48](https://github.com/alkanna/quarry/issues/48)) ([b5e468b](https://github.com/alkanna/quarry/commit/b5e468b5a9242ce07b25a8bffbff82628983936d))

## [0.10.2](https://github.com/alkanna/quarry/compare/v0.10.1...v0.10.2) (2026-11-05)


### Bug Fixes

* hover and press states on every interactive element ([#44](https://github.com/alkanna/quarry/issues/44)) ([be8bf9c](https://github.com/alkanna/quarry/commit/be8bf9caba9baaad3a6340dcc2690dfa8508a122))

## [0.10.1](https://github.com/alkanna/quarry/compare/v0.10.0...v0.10.1) (2026-11-05)


### Bug Fixes

* consistent buttons, cards and spacing across the app ([#42](https://github.com/alkanna/quarry/issues/42)) ([921ed2f](https://github.com/alkanna/quarry/commit/921ed2f0fa8a4f83b11d237ca9208b735f7971ee))

## [0.10.0](https://github.com/alkanna/quarry/compare/v0.9.0...v0.10.0) (2026-11-05)


### Features

* Cmd+F find in big text areas ([#37](https://github.com/alkanna/quarry/issues/37)) ([67d6b56](https://github.com/alkanna/quarry/commit/67d6b56ff83355a6448f02075c98174135198e2d))
* edit full document as JSON from the row menu ([#40](https://github.com/alkanna/quarry/issues/40)) ([afde42f](https://github.com/alkanna/quarry/commit/afde42f71ae86a02fdebfd74655d72a5ed3e8a33))
* light and dark themes ([#41](https://github.com/alkanna/quarry/issues/41)) ([c88157e](https://github.com/alkanna/quarry/commit/c88157eb9074cc6945473997b15b8ea970e51942))


### Bug Fixes

* rerun the edit after allowing writes ([#38](https://github.com/alkanna/quarry/issues/38)) ([fdf6422](https://github.com/alkanna/quarry/commit/fdf642236cb4a69151dd71556b3a397da42ceedd))

## [0.9.0](https://github.com/alkanna/quarry/compare/v0.8.0...v0.9.0) (2026-11-05)


### Features

* add a setting to always allow writes and save edits without a change preview ([#29](https://github.com/alkanna/quarry/issues/29)) ([ca86ed6](https://github.com/alkanna/quarry/commit/ca86ed64758fba3a43726e7212de3d522aa82cef))
* add date shortcuts like #last2months to queries and their autocomplete ([#30](https://github.com/alkanna/quarry/issues/30)) ([40a3967](https://github.com/alkanna/quarry/commit/40a3967f2a562d80f571271e7c77af02852b1201))
* add expand all, collapse all and expand recursively to the document tree menu ([#28](https://github.com/alkanna/quarry/issues/28)) ([38e077b](https://github.com/alkanna/quarry/commit/38e077b68b62e01827a0c6dec1848bee371604db))
* all date shortcuts and date autocomplete ([#33](https://github.com/alkanna/quarry/issues/33)) ([081cdbc](https://github.com/alkanna/quarry/commit/081cdbc5566a9a6dc6528b70f1a7bf768ea432d7))
* pipeline builder with per-stage previews ([#32](https://github.com/alkanna/quarry/issues/32)) ([504e261](https://github.com/alkanna/quarry/commit/504e261aaccc4976a80d95da9772fdecd30c3396))
* show big objects as their size in columns ([#34](https://github.com/alkanna/quarry/issues/34)) ([565c931](https://github.com/alkanna/quarry/commit/565c9314b7391fe34c26396c58c4335b06cddb8c))


### Bug Fixes

* group by and count, validator and settings scrolling ([#36](https://github.com/alkanna/quarry/issues/36)) ([3c31a76](https://github.com/alkanna/quarry/commit/3c31a76236db2b2174858af22637643b0015fc36))
* scrolling performance ([#35](https://github.com/alkanna/quarry/issues/35)) ([61c3c16](https://github.com/alkanna/quarry/commit/61c3c16b1f77a45492fa9772d19a0c70010d6945))

## [0.8.0](https://github.com/alkanna/quarry/compare/v0.7.1...v0.8.0) (2026-11-05)


### Features

* add a query log and fix loading states and error handling ([#26](https://github.com/alkanna/quarry/issues/26)) ([7930afd](https://github.com/alkanna/quarry/commit/7930afd595bf253b0ff57a39fd2bcd685c5ba19b))

## [0.7.1](https://github.com/alkanna/quarry/compare/v0.7.0...v0.7.1) (2026-11-05)


### Bug Fixes

* apply auto indent in the same frame and colour hex id strings as strings ([#24](https://github.com/alkanna/quarry/issues/24)) ([5a5d67b](https://github.com/alkanna/quarry/commit/5a5d67bb2d883fb1e541f4ca3e0409229b28d737))

## [0.7.0](https://github.com/alkanna/quarry/compare/v0.6.1...v0.7.0) (2026-11-05)


### Features

* install the MongoDB Database Tools when mongodump or mongorestore is missing ([#22](https://github.com/alkanna/quarry/issues/22)) ([17f82b9](https://github.com/alkanna/quarry/commit/17f82b9b566bff75ca073b7dd41cbc5ca028730b))

## [0.6.1](https://github.com/alkanna/quarry/compare/v0.6.0...v0.6.1) (2026-11-05)


### Bug Fixes

* scroll the Indexes view horizontally and vertically ([#20](https://github.com/alkanna/quarry/issues/20)) ([eeb705c](https://github.com/alkanna/quarry/commit/eeb705c946ea4f0935f0b607326b16934177d6e7))

## [0.6.0](https://github.com/alkanna/quarry/compare/v0.5.0...v0.6.0) (2026-11-05)


### Features

* copy a query or pipeline as driver code ([#17](https://github.com/alkanna/quarry/issues/17)) ([3064355](https://github.com/alkanna/quarry/commit/3064355b885a558c07f8f5e454cba2aa9bc88c38))
* explain plans for queries and pipelines ([#13](https://github.com/alkanna/quarry/issues/13)) ([85bf6f8](https://github.com/alkanna/quarry/commit/85bf6f8e3bba18a8d9210b6a410b88de4cd28d23))
* index usage, hide/unhide and suggestions in the Indexes view ([#14](https://github.com/alkanna/quarry/issues/14)) ([91fbf75](https://github.com/alkanna/quarry/commit/91fbf757b8f4d6847b82097e1e672a729f9c0ee3))
* keyboard shortcuts for reload, tabs, views, focus and paging, with a shortcut sheet ([#12](https://github.com/alkanna/quarry/issues/12)) ([b4b5079](https://github.com/alkanna/quarry/commit/b4b5079320f06ab16c0571499a2f4ee2f9be0c0a))
* multi-word palette matching and reveal the jump target in the tree ([#10](https://github.com/alkanna/quarry/issues/10)) ([a7ea07e](https://github.com/alkanna/quarry/commit/a7ea07e4ae24962b8f629b71809160948aa367af))
* preview aggregation output after each stage ([#16](https://github.com/alkanna/quarry/issues/16)) ([c704a4f](https://github.com/alkanna/quarry/commit/c704a4f6fe4fe953653847280313268dfb609ac4))
* right-click a property name for the field menu ([#11](https://github.com/alkanna/quarry/issues/11)) ([0d7ca11](https://github.com/alkanna/quarry/commit/0d7ca111dd13e1c616878a8886e857824462469f))
* update matching documents with a preview ([#15](https://github.com/alkanna/quarry/issues/15)) ([5ae6898](https://github.com/alkanna/quarry/commit/5ae68989909d9f0dcaafae8bd1102cd84e76269f))
* view, generate and apply collection validators ([#18](https://github.com/alkanna/quarry/issues/18)) ([91878e0](https://github.com/alkanna/quarry/commit/91878e0acda9de48bd111a9cd44491c08748a1b0))

## [0.5.0](https://github.com/alkanna/quarry/compare/v0.4.0...v0.5.0) (2026-11-04)


### Features

* highlight JSON in the full value and view definition dialogs ([#7](https://github.com/alkanna/quarry/issues/7)) ([59e8dc4](https://github.com/alkanna/quarry/commit/59e8dc4be77b36904b5babfe042830af1e78ffaa))
* reconnect to the last used connection at launch ([#6](https://github.com/alkanna/quarry/issues/6)) ([fe80057](https://github.com/alkanna/quarry/commit/fe80057e0373d84bc680da75c8fcd1a9e03e711c))

## [0.4.0](https://github.com/alkanna/quarry/compare/v0.3.0...v0.4.0) (2026-11-04)


### Features

* add a Cmd+K palette to jump to a connection, database or collec… ([#5](https://github.com/alkanna/quarry/issues/5)) ([ad482ea](https://github.com/alkanna/quarry/commit/ad482ea84873dadc33934ca3e78b08caa3dd9fd7))


### Bug Fixes

* **ci:** build releases without the target cache ([633e99f](https://github.com/alkanna/quarry/commit/633e99f2d4099b44eca4a39479842f1f28aa380e))

## [0.3.0](https://github.com/alkanna/quarry/compare/v0.2.0...v0.3.0) (2026-11-04)


### Features

* show what's new after an update ([#2](https://github.com/alkanna/quarry/issues/2)) ([1f244ce](https://github.com/alkanna/quarry/commit/1f244ced1ad67b027e5e48785905681cb67dcf62))

## [0.2.0](https://github.com/alkanna/quarry/compare/v0.1.0...v0.2.0) (2026-11-04)


### Features

* initial release of Quarry, a native MongoDB workbench ([beb2254](https://github.com/alkanna/quarry/commit/beb2254169ccd168168705c463eadf420b71690f))
