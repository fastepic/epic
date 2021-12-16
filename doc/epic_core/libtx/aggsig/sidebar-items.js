initSidebarItems({"fn":[["add_signatures","Adds signatures"],["calculate_partial_sig","Calculates a partial signature given the signer’s secure key, the sum of all public nonces and (optionally) the sum of all public keys."],["create_secnonce","Creates a new secure nonce (as a SecretKey), guaranteed to be usable during aggsig creation."],["sign_from_key_id","Creates a single-signer aggsig signature from a key id. Generally, this function is used to create transaction kernel signatures for coinbase outputs. Returns `Ok(Signature)` if the signature is valid, or a Signature ErrorKind otherwise"],["sign_single","Just a simple sig, creates its own nonce if not provided"],["sign_with_blinding","Just a simple sig, creates its own nonce, etc"],["verify_completed_sig","Verifies a completed (summed) signature, which must include the message and pubkey sum values that are used during signature creation time to create ‘e’ Returns `Ok(())` if the signature is valid, or a Signature ErrorKind otherwise"],["verify_partial_sig","Verifies a partial signature from a public key. All nonce and public key sum values must be identical to those provided in the call to `calculate_partial_sig`. Returns `Result::Ok` if the signature is valid, or a Signature ErrorKind otherwise"],["verify_single","Verifies an aggsig signature"],["verify_single_from_commit","Simple verification a single signature from a commitment. The public key used to verify the signature is derived from the commit. Returns `Ok(())` if the signature is valid, or a Signature ErrorKind otherwise"]]});