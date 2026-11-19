# lsdocs-transform-nav

Part of [lsdocs](https://github.com/lsdocs), a static-site generator whose
parsers, transformers and renderers are plugins.

`crates/transform-nav` (`lsdocs-transform-nav`, builtin `transform-nav`):
each page's navigation tree (from the source directories, `nav_title`,
`nav_order`, `nav_exclude` front matter) and previous/next links, in its
`nav` metadata.

Its settings and behavior are described in the lsdocs documentation
(https://lsdocs.github.io, "transform-nav").

## Known limitations

- The whole site's tree is sent with every page (see lsdocs-protocol's
  TODO on sharing site-wide data once).

## TODO

- [ ] Nothing open here.
