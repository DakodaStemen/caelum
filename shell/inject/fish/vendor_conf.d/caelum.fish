# Caelum automatic fish integration. Caelum prepends its data directory to
# XDG_DATA_DIRS so fish loads this vendor_conf.d snippet; remove that entry again
# (child shells must not inherit it) and load the integration.
set -l __caelum_root (string replace -r '/fish/vendor_conf\.d$' '' -- (status dirname))
set -gx XDG_DATA_DIRS (string join : (string match -v -- $__caelum_root (string split : -- $XDG_DATA_DIRS)))
test -r $__caelum_root/caelum.fish; and source $__caelum_root/caelum.fish
