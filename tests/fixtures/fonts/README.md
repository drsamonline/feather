# The test face

`DejaVuSans-subset.ttf` is a subset of DejaVu Sans — ASCII plus the em dash,
right single quote, ellipsis and é — cut down from 741 KB to 37 KB with
`fontTools.subset`.

It exists so that a rendering test can be *deterministic*. The same page scored
0% against a baseline rendered on a developer machine and 14% on a CI runner,
because the two machines had different fonts installed. A pixel comparison needs
one face, fixed, in the repository.

Point the viewer at it with:

```sh
FEATHER_FONT=tests/fixtures/fonts/DejaVuSans-subset.ttf feather-viewer --screenshot out.png tests/fixtures/latin.txt
```

## Licence

DejaVu fonts are under the Bitstream Vera licence, which permits modification and
redistribution provided the copyright notice travels with the font. It does, in
the font's own name table, and this notice restates it:

> Copyright (c) 2003 by Bitstream, Inc. All Rights Reserved. Bitstream Vera is a
> trademark of Bitstream, Inc. DejaVu changes are in the public domain.
