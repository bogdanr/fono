AUR
===

`PKGBUILD` here mirrors a typical Arch User Repository recipe. To publish:

    cp PKGBUILD /some/aur/checkout/fono/
    cd /some/aur/checkout/fono
    makepkg --printsrcinfo > .SRCINFO
    git commit -asm "fono $VERSION"
    git push

Update the `sha256sums=('SKIP')` line to the real checksum of the
release tarball before pushing (we ship `SKIP` here because the
tarball hash is only known after tagging a release).
