# Changelog

All notable changes to this project will be documented in this file.

## [0.2.0] - 2026-11-20
<!-- release-base: 2d445fc60f56bca9ea4aaee84d68e72e221c02c4 -->

### 🚀 Features

- *(wallpaper)* Select cached HEICs by catalog ID (#99)

### 🐛 Bug Fixes

- *(wallpaper)* Accept trailing ICC profile padding (#102)

### 📚 Documentation

- Add the Genkan icon and brand guidance (#101)
- Reorganize README and guides by task (#103)

## [0.1.0] - 2026-11-19
<!-- release-base: 466d0415ec2b951d6f562c19086909d4c9f5fd26 -->

### 🚀 Features

- Implement graphical greetd frontend
- Harden greeter authentication and graphics validation (#4)
- Add safe greeter preview and define Tahoe-inspired login flow (#5)
- Establish account-aware greeter interaction foundations (#6)
- *(ui)* Introduce Tahoe-inspired account presentation (#7)
- *(auth)* Clarify authentication and recovery states (#8)
- *(ui)* Unify greeter surface styling (#9)
- *(ui)* Add accessible keyboard focus navigation (#10)
- *(preview)* Verify responsive greeter behavior (#11)
- *(wallpaper)* Add bounded GStreamer playback (#16)
- *(wallpaper)* Add animated Tahoe and Sequoia greeter backgrounds (#17)
- [**breaking**] Establish session-lock foundations (#19)
- *(lock)* Establish compositor lock boundary (#21)
- *(lock)* Add PAM unlock authentication (#23)
- *(lock)* Integrate suspend-ready session locking (#24)
- *(lock)* Add macOS-style authentication screen (#29)
- *(outputs)* Target authentication to one monitor (#31)
- *(wallpaper)* Establish dynamic HEIC format model (#32)
- *(wallpaper)* Parse and decode dynamic HEIC (#33)
- *(wallpaper)* Schedule dynamic HEIC by time (#34)
- *(wallpaper)* Add layer-shell desktop runtime (#35)
- *(wallpaper)* Add optional desktop GeoClue solar scheduling (#36)
- *(wallpaper)* Reuse dynamic HEIC in login and lock (#38)
- *(wallpaper)* Complete RFD 4 phase 7 and fix dynamic HEIC parsing (#45)
- *(design)* Bundle Inter and mirror the design tokens in Rust (#63)
- *(login)* Rebuild the login screen on the macOS design system (#64)
- *(lock)* Rebuild the lock screen on the macOS design system (#67)
- *(design)* Finish shared login and lock presentation (#75)
- *(greeter)* Add compositor placement and matching wallpaper surfaces (#72)
- *(nixos)* Deploy a validated niri greeter and test its lifecycle (#74)
- *(nixos)* Let the niri greeter choose its render device (#77)
- *(avatars)* Package the built-in account pictures (#80)
- *(avatars)* Decode account pictures in a confined process (#81)
- *(avatars)* Show account pictures on the login screen (#82)
- *(avatars)* Show the account picture on the lock screen (#83)
- *(avatars)* Let users choose their account picture (#86)
- *(assets)* List and preview original-resolution wallpapers (#89)
- *(assets)* Download original wallpapers on demand (#90)
- *(assets)* Streamline previews with download progress (#91)
- *(wallpaper)* Provision validated shared media (#92)
- *(wallpapers)* Expand solar and aerial catalog (#93)
- *(wallpapers)* Use Apple’s macOS Tahoe catalog (#94)

### 🐛 Bug Fixes

- *(auth)* Cancel abandoned greetd sessions (#2)
- Address medium review findings (#3)
- *(ui)* Verify accessible contrast across renderers (#12)
- *(lock)* Harden session-lock runtime (#22)
- *(wallpaper)* Prevent animated frame flicker (#25)
- *(wallpaper)* Decode padded-coded-picture HEICs and validate the wallpaper manifest (#52)
- *(wallpaper)* Decode real ICC-profile and 6K dynamic HEICs (#53)
- Address six authentication and output audit findings (#58)
- *(login)* Draw icons as images so the software renderer shows them (#66)
- *(deps)* Update iced_tiny_skia to 0.14.1 (#71)
- *(design)* Soften wallpaper text and keep the caret inside the field (#78)
- *(avatars)* Commit the rendered pictures instead of rendering at build (#87)
- *(wallpapers)* Show verified aerial resolutions offline (#96)

### 🚜 Refactor

- *(nix)* Extract per-system configuration (#18)
- *(auth)* Separate conversation model (#20)
- *(wallpaper)* Pass mapped solar points directly (#37)

### 📚 Documentation

- Add development and security guidance
- *(rfd)* Finalize greeter design record (#14)
- *(rfd)* Propose animated 4K wallpaper (#15)
- *(rfd)* Correct RFD status drift and colorize the checker (#51)
- *(rfd)* Record the wallpaper object layout and convert checklists to AsciiDoc (#56)
- *(rfd)* Propose RFD 5 design system and RFD 6 niri greeter (#60)
- *(design)* Add the macOS-based design system (#61)
- *(rfd)* Amend RFD 1 for macOS fidelity and publish RFDs 5 and 6 (#62)
- *(deployment)* Add a niri section (#69)
- Refresh deferred RFD work and accessibility status (#70)
- *(rfd)* Propose RFD 7 for account pictures (#79)
- *(avatars)* Complete RFD 7 records (#88)
- Refresh deployment and verification guidance (#97)

### ⚡ Performance

- *(lock)* Avoid copying wallpaper on input (#27)
- *(lock)* Optimize HiDPI frame rendering (#30)

### 🧪 Testing

- Expand greeter protocol coverage (#1)
- *(preview)* Add durable greeter acceptance references (#13)
- *(lock)* Add session-lock system verification (#26)
- *(lock)* Add deterministic lock preview (#28)
- *(lock)* Harden the descendant pid marker wait (#42)
- *(lock)* Keep a seat keyboard so injected keypresses are not lost (#43)
- Add property contracts and bounded parser fuzzing (#65)
- *(sessions)* Check the strict Exec rule against the parsed value (#85)

### ⚙️ Miscellaneous Tasks

- *(nix)* Add reproducible development environment
- *(dev)* Add Makefile commands
- Add Linux workflow
- Align workflow with sibling Rust projects
- Add conventional commit tooling
- *(orb)* Prepare cached Nix development environments (#59)
- *(release)* Publish SemVer releases with prebuilt packages (#95)
