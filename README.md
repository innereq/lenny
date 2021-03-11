**NOTE:** this project is maintained in @commagray's spare time. It's not well tested nor used, better to fork it.

<p align="center">
  <img width=200px height=200px src="https://raw.githubusercontent.com/innereq/lenny-ui/downstream/src/assets/icons/favicon.svg">
  <h3 align="center">( ͡° ͜ʖ ͡°)</h3>
</p>

[![](https://github.com/innereq/lenny/workflows/Continuous%20Integration/badge.svg)](https://github.com/innereq/lenny/actions?query=workflow%3A"Continuous+Integration") [![](https://github.com/innereq/lenny/workflows/Build%20Lenny%20on%20Push%20and%20Daily/badge.svg)](https://github.com/innereq/lenny/actions?query=workflow%3A"Build+Lenny+on+Push+and+Daily")

**Lenny** is a fork of a link aggregator — [Lemmy](https://github.com/LemmyNet/lemmy). Sadly, it only exist because of disrespectful behavior of the original author.

To maintain stability, this fork has a little no changes, but:
- the main reason, completely removed “slur filter” — the ugliest way to block words;
- a bit cleaned up UI — removed ~~“sponsors” page~~ (upstreamed!) and useless right-down panel;
- new default themes — Pleroma, based on themes of the [Pleroma project](https://pleroma.social);
- ~~allowed `<sub>text</sub>` and `<sup>text</sup>` HTML tags as `~text~` and `^text^`~~ (upstreamed!);
- a muffin logo (by [MLP Vector Club](https://mlpvector.club)).

# The Lemmy Problem

`static ref SLUR_REGEX: Regex = RegexBuilder::new(r"(fag(g|got|tard)?|maricos?|cock\s?sucker(s|ing)?|\bn(i|1)g(\b|g?(a|er)?(s|z)?)\b|dindu(s?)|mudslime?s?|kikes?|mongoloids?|towel\s*heads?|\bspi(c|k)s?\b|\bchinks?|niglets?|beaners?|\bnips?\b|\bcoons?\b|jungle\s*bunn(y|ies?)|jigg?aboo?s?|\bpakis?\b|rag\s*heads?|gooks?|cunts?|bitch(es|ing|y)?|puss(y|ies?)|twats?|feminazis?|whor(es?|ing)|\bslut(s|t?y)?|\btr(a|@)nn?(y|ies?)|ladyboy(s?)|\b(b|re|r)tard(ed)?s?)").case_insensitive(true).build().unwrap();`

https://github.com/LemmyNet/lemmy/commit/1c0cc78f3f6d191aa384d8702016564625d51269

> We are never going to remove the slur filter completely (or add an option to that effect), because we dont want to make it easy for right-wingers to use Lemmy. We can talk about removing or changing specific words, but in general I dont think there is anything wrong with writing “b*tch” or something like that.

https://github.com/LemmyNet/lemmy/pull/816#issuecomment-644694838

> I’ll have to think about this. Hard-coding it means I don’t have to do a database migration every time someone comes up with a new slur. And putting it in a DB table means someone could very easily remove it by deleting every row of that table, which isn’t good. I want to make it very difficult for racist trolls to use the most updated version of Lemmy.

https://github.com/LemmyNet/lemmy/issues/622#issuecomment-608707278

This is bullshit.

# Development

For development environment you should have installed Rust toolchain and Node.js.

## Backend

```bash
pacman -S rustup nodejs yarn npm
rustup update nightly
git clone https://github.com/innereq/lenny && cd lenny
cargo build
```

To build a production container you could use Podman or Docker.

```bash
podman build -t lenny -f ./docker/prod/Dockerfile .
```

## Frontend

Frontend now is a separated standalone project.

```bash
git clone https://github.com/innereq/lenny-ui && cd lenny-ui
yarn
yarn build
```

To build a production container you could use Podman or Docker.

```bash
podman build -t lenny-ui .
```

# Deploying

You can use our prebuilt container or build your own. Remember to choose a tag.

```
podman pull podman pull ghcr.io/innereq/containers/lenny
podman pull podman pull ghcr.io/innereq/containers/lenny-ui
```
