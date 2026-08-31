################################################################################
#
# podd
#
# Stages the podd control daemon + web UI into the rootfs. The artifacts are
# produced OUTSIDE Buildroot for reproducibility and to reuse the existing Nix
# cross-build:
#
#     nix build .#podd-aarch64   -> result-podd/bin/podd  (static aarch64-musl)
#     nix build .#ui             -> result-ui             (built SPA)
#
# Pass their locations in when invoking Buildroot, e.g.:
#
#     make PODD_OVERRIDE_SRCDIR_PODD=... \
#          PODD_BIN=$PWD/../result-podd/bin/podd \
#          PODD_UI_DIR=$PWD/../result-ui
#
# (or export PODD_BIN / PODD_UI_DIR in the environment). This package is a
# no-download "install-only" package: it has no source of its own.
#
################################################################################

PODD_VERSION = 0.0.1
PODD_SITE_METHOD = local
# `local` with an empty SITE just means "nothing to fetch"; we install from the
# externally-built artifacts pointed at by $(PODD_BIN) / $(PODD_UI_DIR).
PODD_SITE = $(BR2_EXTERNAL_PODD_PATH)/package/podd
PODD_LICENSE = GPL-3.0-or-later
PODD_LICENSE_FILES =

# Resolved from the environment; error out early if unset so the build fails
# with a clear message instead of installing an empty rootfs.
PODD_BIN ?=
PODD_UI_DIR ?=

define PODD_CHECK_ARTIFACTS
	if [ ! -x "$(PODD_BIN)" ]; then \
		echo "podd.mk: PODD_BIN is unset or not executable ('$(PODD_BIN)')." >&2; \
		echo "  build it with: nix build .#podd-aarch64" >&2; \
		exit 1; \
	fi; \
	if [ ! -d "$(PODD_UI_DIR)" ]; then \
		echo "podd.mk: PODD_UI_DIR is unset or missing ('$(PODD_UI_DIR)')." >&2; \
		echo "  build it with: nix build .#ui" >&2; \
		exit 1; \
	fi
endef
PODD_PRE_INSTALL_TARGET_HOOKS += PODD_CHECK_ARTIFACTS

define PODD_INSTALL_TARGET_CMDS
	$(INSTALL) -D -m 0755 $(PODD_BIN) $(TARGET_DIR)/usr/bin/podd
	$(INSTALL) -D -m 0755 $(BR2_EXTERNAL_PODD_PATH)/package/podd/podd-launch \
		$(TARGET_DIR)/usr/bin/podd-launch
	# Clean first: TARGET_DIR persists across builds and the UI bundle names are
	# content-hashed, so stale assets from earlier builds accumulate otherwise.
	rm -rf $(TARGET_DIR)/usr/share/podd/ui
	mkdir -p $(TARGET_DIR)/usr/share/podd/ui
	# The UI comes from the Nix store, whose files/dirs are read-only (0444/0555).
	# `cp -a` would preserve those perms, so Buildroot's later fakeroot cleanup
	# can't remove them ("Permission denied"). Dereference symlinks and normalize
	# to writable-by-owner, world-readable.
	cp -rL $(PODD_UI_DIR)/. $(TARGET_DIR)/usr/share/podd/ui/
	chmod -R u+rwX,go=rX $(TARGET_DIR)/usr/share/podd/ui
endef

define PODD_INSTALL_INIT_SYSTEMD
	$(INSTALL) -D -m 0644 $(BR2_EXTERNAL_PODD_PATH)/package/podd/podd.service \
		$(TARGET_DIR)/usr/lib/systemd/system/podd.service
	mkdir -p $(TARGET_DIR)/etc/systemd/system/multi-user.target.wants
	ln -sf ../../../../usr/lib/systemd/system/podd.service \
		$(TARGET_DIR)/etc/systemd/system/multi-user.target.wants/podd.service
endef

$(eval $(generic-package))
