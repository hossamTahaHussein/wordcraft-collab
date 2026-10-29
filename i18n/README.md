# Interface translations

WordCraft's interface catalogues (`wordcraft.tsv`, plus the shared `common.tsv`) for each
language, copied from [storytold/craft-i18n](https://github.com/storytold/craft-i18n) at commit
`19358491340b9f33d618673de6bcd40108d9a775`, under the MIT License or the Apache License 2.0 (`LICENSE-MIT`, `LICENSE-APACHE`
here). The build embeds them (`crates/ui-egui/build.rs`); `CRAFT_I18N_DIR` still points it at
another craft-i18n checkout. Strings missing from a catalogue show in English.
