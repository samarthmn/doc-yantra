# Heading level 1

## Heading level 2

### Heading level 3

#### Heading level 4

##### Heading level 5

###### Heading level 6

## Task lists

- [ ] unchecked task
- [x] checked task
  - [ ] nested unchecked task
  - [x] nested checked task

## Strikethrough

This word is ~~gone~~ and this one stays.

## Footnotes

Here is a footnote reference[^1] and a second one[^long].

[^1]: The first footnote definition.
[^long]: The second footnote definition, which is somewhat longer and wraps
    onto a continuation line.

## Autolinks

Visit www.example.com or https://example.org/path?q=1 or write to
user@example.com for details.

## Tables

| Left | Center | Right |
| :--- | :----: | ----: |
| a    |   b    |     1 |
| ccc  |  ddd   |  22.5 |

## Nested lists

1. first ordered
2. second ordered
   - inner bullet
   - another bullet
     1. deep ordered
     2. deeper ordered
3. third ordered

## Code

Inline `code span` here.

```js
function greet(name) {
  return `Hello, ${name}!`;
}
```

## Blockquote

> A quoted paragraph.
>
> > A nested quote with `code` inside.

## Horizontal rule

---

## Links

An [inline link](https://example.com "Example title") and a
[reference link][ref].

[ref]: https://example.net

## Image reference

![x](y.png)

## Raw HTML

Raw <b>html</b> should follow the tagfilter policy: <script>alert(1)</script>
and an entity: &copy; &amp; &lt;tag&gt;.
