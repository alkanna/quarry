# Developer ID build for the maintainer's Mac: needs the cert in the keychain and the "quarry" notarytool profile.
release-mac:
	QUARRY_SIGN_ID=167E682A9DE81BC21DED6018673F23E6EA9DB831 QUARRY_NOTARY_ARGS="--keychain-profile quarry" ./bundle.sh

.PHONY: release-mac
