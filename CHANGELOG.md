# Changelog for egg-mode

## [0.2.0] - 2016-08-08
### Added
- Entity structs, so you can parse URL's from user bios (and from tweets in the future)

### Changed
- Added entity information to the user struct
- Removed dependency on the `time` crate (Thanks, serprex!)

## [0.1.1] - 2016-08-04
### Changed
- Relicense with Apache2 while I figure out how to make LGPL work with Rust

## [0.1.0] - 2016-08-04
### Added
- Initial version
- Auth methods
- User lookup, search, friend/follower list
- "basic" example showing various user lookups
- "reciprocal" example showing the users you mutually follow