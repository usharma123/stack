---- MODULE BundleIntegrity ----
\* What a compile accepts from a bundle, and what it sends a registry (src/project.rs:
\* compile_checked, check_committed_artifacts; src/source.rs: fetch_git, fetch_oci, fetch_path;
\* src/hash.rs: hash_keeping, list_files, excluded, first_excluded; src/manifest.rs:
\* parse_bundle; src/compose.rs: bin_dir, LoadedBundle::new, compose; src/oci.rs: Client::send,
\* authenticate_with, CredentialEnv::grant, trusted_realm, resolve, pull, verify_cached, layer,
\* extract, Evidence; src/artifacts.rs: Policy::check_host).
\*
\* One project with one [[use]] bundle, from a local directory, a Git repository or an OCI
\* registry, and a sequence of compiles: `compile`, `compile --update`, and `compile --locked`,
\* which is also what every locked operation runs first (install, up, exec, run, status, MCP
\* tools alike). Each compile is a sequence of steps. Between any two, the local directory can be
\* edited, the upstream ref or tag moved, stack.lock edited, and a cached tree changed by another
\* process running as the same user.
\*
\* Trust. The operator configures, in the environment, the origin registry credentials are bound
\* to (STACK_OCI_USERNAME, STACK_OCI_PASSWORD, STACK_OCI_REGISTRY) and the approved token services
\* (STACK_OCI_AUTH_REALMS): constants of a behavior. The project chooses the bundle reference (so
\* the registry origin R), [lock] artifacts and stack.lock, which is data checked before use.
\* Untrusted: the bundle (its tree, bundle.toml, paths.bin), whatever a registry answers
\* (redirects, challenges and their realms, token replies, manifest bytes, the layer digest a
\* manifest names, blobs), and cached trees this Stack did not install.
\*
\* A compile's promises are about the snapshot it hashed: the manifest it parses, the pins it
\* writes or checks, and what paths.bin names when it composes. The bundle's other files are read
\* when used, from the local directory or the cache, without an immutable copy;
\* `ToolsUsedAreHashed` states the stronger property only to show it fails (LIMIT config).
EXTENDS Naturals, FiniteSets

CONSTANTS Faults, Compiles, Events

Fault(f) == f \in Faults

Transports == {"local", "git", "oci"}
Modes == {"compile", "locked", "update"}
Bad == 9                                   \* a pin stack.lock records that is not a commit id or digest
Slots == {"absent", "evidence", "same", "diff", "excluded"}
NoLock == [on |-> FALSE, pin |-> 0, hash |-> 0]
Idle == [step |-> "idle", mode |-> "compile", pin |-> 0, lockPin |-> 0, lockHash |-> 0,
         lockOn |-> FALSE, hashed |-> 0, manifest |-> 0, tree |-> 0, slot |-> "absent",
         linkAtHash |-> FALSE, binAtLoad |-> "plain"]
NoTalk == [at |-> "R", challenged |-> FALSE, redirects |-> 0, token |-> "none"]

VARIABLES
    transport,   \* where the bundle comes from
    excl,        \* (OCI) the published layer holds a directory .git/.stack would fold to (.Git)
    cred,        \* operator credentials: "none", "bound" (to R), "elsewhere" (to Y), "unbound"
    policy,      \* [lock] artifacts: "best", "requiredGap" (a pin unchecked), "requiredUnlisted"
    shape,       \* the bundle's tree holds a link or special file somewhere ("link") or not
    binNow,      \* its paths.bin entry as on disk now: plain, `..`, a link, or `.Stack/bin`
    upstream,    \* what the Git ref or OCI tag resolves to now (1 or 2)
    localVer,    \* the local directory's content now (1 or 2)
    lock,        \* stack.lock's entry: on, pin (commit or digest 1, 2, or Bad), hash
    cache,       \* (OCI) the cache slot of each digest
    c,           \* the running compile
    talk,        \* its registry conversation
    accepted,    \* the last compile that finished, and the content it hashed
    compiles, events,
    manifestUnhashed,   \* history: a compile parsed a manifest other than the bytes it hashed
    unhashedAccepted,   \* history: a compile accepted a tree with a link or special file in it
    binOutside,         \* history: a compile put a directory outside the hashed bundle on PATH
    notDigest,          \* history: an OCI compile accepted a tree other than its digest's layer
    excludedAccepted,   \* history: an OCI compile accepted a tree holding an excluded directory
    badNamed,           \* history: a value not checked to be a digest named a path or request
    lockedDrift,        \* history: a locked compile accepted pins other than the lock it read
    pinMoved,           \* history: a compile other than --update moved a locked pin
    unlistedRan,        \* history: a locked operation on an unlisted host fetched or reached mise
    uncovered,          \* history: under required, a locked operation reached mise, or a compile
                        \*          wrote, with a pin unchecked
    credsTo,            \* history: origins the operator's credentials were sent to
    authCross           \* history: an Authorization header went to an origin it was not for

vars == <<transport, excl, cred, policy, shape, binNow, upstream, localVer, lock, cache, c, talk,
          accepted, compiles, events, manifestUnhashed, unhashedAccepted, binOutside, notDigest,
          excludedAccepted, badNamed, lockedDrift, pinMoved, unlistedRan, uncovered, credsTo,
          authCross>>
static == <<transport, excl, cred, policy>>
world == <<shape, binNow, upstream, localVer, lock, cache>>
flags == <<manifestUnhashed, unhashedAccepted, binOutside, notDigest, excludedAccepted, badNamed,
           lockedDrift, pinMoved, unlistedRan, uncovered, credsTo, authCross>>

Init ==
    /\ transport \in Transports
    /\ excl \in (IF transport = "oci" THEN {"none", "folded"} ELSE {"none"})
    /\ cred \in (IF transport = "oci" THEN {"none", "bound", "elsewhere", "unbound"} ELSE {"none"})
    /\ policy \in {"best", "requiredGap", "requiredUnlisted"}
    /\ shape \in {"regular", "link"}
    \* As bundle.toml writes it; a link is a directory replaced after a hash saw the tree.
    /\ binNow \in {"plain", "dotdot", "folded"}
    /\ upstream = 1 /\ localVer = 1
    \* stack.lock as committed: none, or pinning version 1 with its content hash.
    /\ lock \in {NoLock, [on |-> TRUE, pin |-> IF transport = "local" THEN 0 ELSE 1, hash |-> 1]}
    \* The cache slot of digest 1 as an earlier Stack filled it, or as restored from elsewhere.
    /\ cache \in (IF transport = "oci"
                  THEN {[d \in {1, 2} |-> IF d = 1 THEN s ELSE "absent"] : s \in Slots}
                  ELSE {[d \in {1, 2} |-> "absent"]})
    /\ c = Idle /\ talk = NoTalk
    /\ accepted = [on |-> FALSE, hash |-> 0]
    /\ compiles = Compiles /\ events = Events
    /\ manifestUnhashed = FALSE /\ unhashedAccepted = FALSE /\ binOutside = FALSE
    /\ notDigest = FALSE /\ excludedAccepted = FALSE /\ badNamed = FALSE /\ lockedDrift = FALSE
    /\ pinMoved = FALSE /\ unlistedRan = FALSE /\ uncovered = FALSE /\ credsTo = {}
    /\ authCross = FALSE

Refuse == c' = Idle /\ talk' = NoTalk
Oci == transport = "oci"

---------------------------------------------------------------------------------------------
\* A compile's steps.

\* compile_checked: locked mode needs stack.lock (lock_outdated), then Policy::check_host before
\* any bundle fetch or provider call (974bdf4; before it only install and up refused). The pin
\* is stack.lock's commit or digest in compile and locked mode, checked to be one (lock_invalid,
\* 9a97f0e), and the upstream ref or tag resolved otherwise. Locked mode never resolves.
Begin(mode) ==
    /\ c.step = "idle" /\ compiles > 0 /\ compiles' = compiles - 1
    /\ LET locked == mode = "locked"
           unlisted == locked /\ policy = "requiredUnlisted"
           usesLock == mode # "update" /\ lock.on /\ transport # "local"
           pin == IF transport = "local" THEN 0 ELSE IF usesLock THEN lock.pin ELSE upstream
       IN IF (locked /\ ~lock.on) \/ (unlisted /\ ~Fault("HostCheckOnlyInInstall"))
          THEN Refuse /\ UNCHANGED flags
          ELSE IF pin = Bad
          \* Unchecked, it names a cache directory and a request (git or the registry).
          THEN /\ Refuse
               /\ badNamed' = (badNamed \/ Fault("UncheckedDigests"))
               /\ unlistedRan' = (unlistedRan \/ (unlisted /\ Fault("UncheckedDigests")))
               /\ UNCHANGED <<manifestUnhashed, unhashedAccepted, binOutside, notDigest,
                              excludedAccepted, lockedDrift, pinMoved, uncovered, credsTo, authCross>>
          ELSE /\ c' = [Idle EXCEPT !.step = IF Oci THEN "registry" ELSE "fetch", !.mode = mode,
                                    !.pin = pin, !.lockOn = lock.on, !.lockPin = lock.pin,
                                    !.lockHash = lock.hash]
               /\ talk' = NoTalk
               /\ unlistedRan' = (unlistedRan \/ unlisted)
               /\ UNCHANGED <<manifestUnhashed, unhashedAccepted, binOutside, notDigest,
                              excludedAccepted, badNamed, lockedDrift, pinMoved, uncovered,
                              credsTo, authCross>>
    /\ UNCHANGED <<static, world, accepted, events>>

---------------------------------------------------------------------------------------------
\* One OCI fetch's registry conversation (Client::send, authenticate_with), whatever the
\* registry answers. credential_origin is the first request's origin: R.

\* Whether the fetch asks the registry anything: a tag to resolve, a slot to fill, or a cached
\* tree no lock pin or evidence vouches for (verify_cached).
NeedsRegistry ==
    \/ c.mode = "update" \/ ~c.lockOn
    \/ cache[c.pin] = "absent"
    \/ /\ cache[c.pin] \in {"same", "diff", "excluded"}
       /\ ~(c.lockOn /\ c.lockPin = c.pin)
       /\ ~Fault("ReuseByName")

\* Each request carries Authorization only to the origin its token was issued for. Before b727b0d
\* the HTTP agent followed redirects itself, carrying the header along.
Carry ==
    authCross' = (authCross \/ (talk.token \notin {"none", talk.at} /\ Fault("AuthFollowsRedirects")))

\* Every request answered: the fetch goes on (a redirect target may serve it, e.g. a CDN).
RegistryDone ==
    /\ c.step = "registry"
    /\ IF NeedsRegistry THEN Carry ELSE UNCHANGED authCross
    /\ c' = [c EXCEPT !.step = "fetch"]
    /\ UNCHANGED <<static, world, talk, accepted, compiles, events, manifestUnhashed,
                   unhashedAccepted, binOutside, notDigest, excludedAccepted, badNamed,
                   lockedDrift, pinMoved, unlistedRan, uncovered, credsTo>>

\* A redirect (at most one here): the next request goes to the location, unchallenged.
RegistryRedirect(to) ==
    /\ c.step = "registry" /\ NeedsRegistry /\ talk.redirects = 0
    /\ Carry
    /\ talk' = [talk EXCEPT !.at = to, !.challenged = FALSE, !.redirects = 1]
    /\ UNCHANGED <<static, world, c, accepted, compiles, events, manifestUnhashed,
                   unhashedAccepted, binOutside, notDigest, excludedAccepted, badNamed,
                   lockedDrift, pinMoved, unlistedRan, uncovered, credsTo>>

\* CredentialEnv::grant for the registry origin R: the credentials when they are bound to R,
\* anonymous when none are set or they are bound elsewhere, an error when they are set without a
\* valid binding (oci_credentials_unbound). Before 08600bb they went to whichever registry the
\* reference named whenever it challenged.
Grant ==
    CASE cred = "none" -> "anonymous"
      [] cred = "bound" \/ Fault("CredentialsToAnyChallenger") -> "credentials"
      [] cred = "elsewhere" -> "anonymous"
      [] OTHER -> "error"

\* A 401 and its challenge, answered once per request target, and only at R itself ("refusing
\* authentication at a redirected origin"). Basic: the credentials as the token, or
\* unauthorized. Bearer: the realm must be R or approved (A), never U; the token service is
\* asked with the credentials only under a credentials grant, and may not redirect. The token
\* is kept for R.
RegistryChallenge(kind, tokenReply) ==
    /\ c.step = "registry" /\ NeedsRegistry /\ ~talk.challenged
    /\ Carry
    /\ LET realm == CASE kind = "bearerR" -> "R" [] kind = "bearerA" -> "A" [] OTHER -> "U"
           g == Grant
           bearer == kind # "basic"
           asked == bearer /\ talk.at = "R" /\ realm # "U" /\ g # "error"
           follows == tokenReply = "redirect" /\ Fault("TokenFollowsRedirect")
           ok == talk.at = "R" /\ g # "error"
                 /\ IF bearer THEN realm # "U" /\ (tokenReply = "token" \/ follows)
                    ELSE g = "credentials"
       IN /\ credsTo' = credsTo \cup
                (IF g # "credentials" \/ talk.at # "R" THEN {}
                 ELSE IF ~bearer THEN {"R"}
                 ELSE IF ~asked THEN {}
                 ELSE {realm} \cup (IF follows THEN {"Z"} ELSE {}))
          /\ IF ok THEN talk' = [talk EXCEPT !.challenged = TRUE, !.token = "R"] /\ UNCHANGED c
             ELSE Refuse
    /\ UNCHANGED <<static, world, accepted, compiles, events, manifestUnhashed, unhashedAccepted,
                   binOutside, notDigest, excludedAccepted, badNamed, lockedDrift, pinMoved,
                   unlistedRan, uncovered>>

\* An error status, a second 401 on one target, a second redirect: the fetch fails.
RegistryFails ==
    /\ c.step = "registry" /\ NeedsRegistry
    /\ Refuse
    /\ UNCHANGED <<static, world, accepted, compiles, events, flags>>

---------------------------------------------------------------------------------------------
\* Fetch and hash.

\* What a slot holds: its digest's layer content, or other content (0).
SlotContent(s, d) == IF s = "diff" THEN 0 ELSE d

\* fetch_path and fetch_git: hash_keeping lists the tree, refusing links and special files
\* (612d24a; before, it skipped them), and keeps bundle.toml's bytes from the read it hashed.
\* Git materializes the locked or resolved commit; a lock pinning that commit must match its
\* content hash. A locked local bundle must hash as stack.lock records (lock_outdated).
\* fetch_oci: pull fills an absent slot from the manifest and its layer, each verified against
\* its digest and the layer's digest checked before it is asked for (`layerOk`: the registry's
\* manifest names a well-formed digest and serves bytes matching both); extract refuses links
\* and excluded names, folded (9a97f0e; before, exactly); evidence is written before the tree is
\* published. A present slot holding an excluded directory is refused (first_excluded); else it
\* is reused when stack.lock pins this digest with this content hash, or its evidence matches,
\* or verify_cached finds it is the layer's (then recording evidence). Before 9a97f0e a cached
\* tree was reused by its directory name.
Fetch(layerOk) ==
    /\ c.step = "fetch"
    /\ LET slot == IF Oci THEN cache[c.pin] ELSE "absent"
           pulled == Oci /\ slot = "absent"
           lockPins == c.lockOn /\ c.lockPin = c.pin
           verifying == Oci /\ ~pulled /\ slot # "evidence" /\ ~lockPins /\ ~Fault("ReuseByName")
           fetchesLayer == pulled \/ verifying
           folds == ~Fault("ExactExcluded")
           tree == CASE transport = "local" -> localVer
                     [] transport = "git" -> c.pin
                     [] OTHER -> IF pulled THEN c.pin ELSE SlotContent(slot, c.pin)
           linkSeen == shape = "link" /\ (~Fault("SkipLinks") \/ pulled)
           exclSeen == Oci /\ folds /\ ((pulled /\ excl = "folded") \/ slot = "excluded")
           layerBad == fetchesLayer /\ ~layerOk
           matches == ~Oci \/ pulled \/ Fault("ReuseByName") \/ slot = "evidence"
                      \/ (lockPins /\ c.lockHash = tree) \/ (verifying /\ tree = c.pin)
           mismatch == transport # "local" /\ lockPins /\ c.lockHash # tree
           outdated == transport = "local" /\ c.mode = "locked" /\ c.lockHash # tree
           installed == IF excl = "folded" THEN "excluded" ELSE "evidence"
       IN /\ fetchesLayer \/ layerOk
          \* A bad layer digest is refused before it is asked for; unchecked (UncheckedDigests),
          \* it names a request first, and the bytes then fail their digest.
          /\ IF linkSeen \/ exclSeen \/ layerBad \/ ~matches \/ mismatch \/ outdated
             THEN /\ Refuse /\ UNCHANGED cache
                  /\ badNamed' = (badNamed \/ (layerBad /\ Fault("UncheckedDigests")))
             ELSE /\ c' = [c EXCEPT !.step = "parse", !.hashed = tree, !.tree = tree,
                                    !.slot = IF pulled THEN installed ELSE slot,
                                    !.linkAtHash = shape = "link"]
                  /\ cache' = CASE pulled -> [cache EXCEPT ![c.pin] = installed]
                                [] verifying /\ slot = "same" -> [cache EXCEPT ![c.pin] = "evidence"]
                                [] OTHER -> cache
                  /\ UNCHANGED <<talk, badNamed>>
    /\ UNCHANGED <<static, shape, binNow, upstream, localVer, lock, accepted, compiles, events,
                   manifestUnhashed, unhashedAccepted, binOutside, notDigest, excludedAccepted,
                   lockedDrift, pinMoved, unlistedRan, uncovered, credsTo, authCross>>

\* parse_bundle: from the bundle.toml bytes hash_keeping kept. Before 9a97f0e the file was read
\* again from the tree (read_bundle), as it is by then.
Parse ==
    /\ c.step = "parse"
    /\ c' = [c EXCEPT !.step = "load", !.manifest =
              IF ~Fault("ReadManifestAgain") THEN c.hashed
              ELSE CASE transport = "local" -> localVer
                     [] transport = "oci" -> SlotContent(cache[c.pin], c.pin)
                     [] OTHER -> c.hashed]
    /\ UNCHANGED <<static, world, talk, accepted, compiles, events, flags>>

\* bin_dir, at LoadedBundle::new and again in compose, on the tree as it is: a relative path of
\* plain names, none `..` or excluded (folded), each a real directory, not a link. Before
\* 612d24a containment was lexical after following links (`..` and links passed); before 9a97f0e
\* names were compared exactly (`.Stack/bin`, which hashing skips, passed).
BinOk(k) ==
    CASE k = "plain" -> TRUE
      [] k \in {"dotdot", "link"} -> Fault("LexicalBin")
      [] OTHER -> Fault("ExactExcluded")

Load ==
    /\ c.step = "load"
    /\ IF BinOk(binNow)
       THEN c' = [c EXCEPT !.step = "compose", !.binAtLoad = binNow] /\ UNCHANGED talk
       ELSE Refuse
    /\ UNCHANGED <<static, world, accepted, compiles, events, flags>>

\* compose (bin_dir again), then: locked, the pins must be the lock's it read (lock_outdated) and,
\* under required, every pin checked (check_committed_artifacts) before the preflight or the
\* command reaches mise; compile and --update, under required, unchecked pins fail before
\* anything is written (unlocked_error), else stack.lock is written.
Finish ==
    /\ c.step = "compose"
    /\ LET bin == IF Fault("NoComposeRecheck") THEN c.binAtLoad ELSE binNow
           locked == c.mode = "locked"
           pinsChanged == c.pin # c.lockPin \/ c.hashed # c.lockHash \/ ~c.lockOn
           gap == policy = "requiredGap"
       IN IF ~BinOk(bin) \/ (locked /\ pinsChanged) \/ (gap /\ ~(locked /\ Fault("CoverageOnlyInInstall")))
          THEN Refuse /\ UNCHANGED <<lock, accepted, flags>>
          ELSE /\ lock' = IF locked THEN lock ELSE [on |-> TRUE, pin |-> c.pin, hash |-> c.hashed]
               /\ accepted' = [on |-> TRUE, hash |-> c.hashed]
               /\ c' = Idle /\ talk' = NoTalk
               /\ manifestUnhashed' = (manifestUnhashed \/ c.manifest # c.hashed)
               /\ unhashedAccepted' = (unhashedAccepted \/ c.linkAtHash)
               \* What is on disk when it composes, whatever the code checked.
               /\ binOutside' = (binOutside \/ binNow # "plain")
               /\ notDigest' = (notDigest \/ (Oci /\ c.tree # c.pin))
               /\ excludedAccepted' = (excludedAccepted \/ (Oci /\ c.slot = "excluded"))
               /\ lockedDrift' = (lockedDrift \/ (locked /\ (c.pin # c.lockPin \/ c.hashed # c.lockHash)))
               /\ pinMoved' = (pinMoved \/ (c.mode # "update" /\ c.lockOn /\ transport # "local"
                                             /\ c.pin # c.lockPin))
               /\ uncovered' = (uncovered \/ gap)
               /\ UNCHANGED <<badNamed, unlistedRan, credsTo, authCross>>
    /\ UNCHANGED <<static, shape, binNow, upstream, localVer, cache, compiles, events>>

---------------------------------------------------------------------------------------------
\* The world between steps.

Event == events > 0 /\ events' = events - 1

\* The local directory edited: its content, and perhaps its bin directory replaced by a link.
LocalEdit ==
    /\ Event /\ transport = "local" /\ localVer = 1
    /\ localVer' = 2
    /\ \/ UNCHANGED <<binNow, shape>>
       \/ binNow' = "link" /\ shape' = "link"
    /\ UNCHANGED <<static, upstream, lock, cache, c, talk, accepted, compiles, flags>>

UpstreamMoves ==
    /\ Event /\ transport # "local" /\ upstream = 1
    /\ upstream' = 2
    /\ UNCHANGED <<static, shape, binNow, localVer, lock, cache, c, talk, accepted, compiles, flags>>

\* stack.lock edited by hand, or by a bad merge: a pin that is not a commit id or digest.
LockEdited ==
    /\ Event /\ lock.on /\ transport # "local" /\ lock.pin # Bad
    /\ lock' = [lock EXCEPT !.pin = Bad]
    /\ UNCHANGED <<static, shape, binNow, upstream, localVer, cache, c, talk, accepted, compiles, flags>>

\* Another process running as the user changes a cached tree.
CacheChanged ==
    /\ Event /\ Oci
    /\ \E d \in {1, 2} : cache[d] \in {"evidence", "same"} /\ cache' = [cache EXCEPT ![d] = "diff"]
    /\ UNCHANGED <<static, shape, binNow, upstream, localVer, lock, c, talk, accepted, compiles, flags>>

Done ==
    /\ c.step = "idle" /\ compiles = 0
    /\ UNCHANGED vars

Next ==
    \/ \E m \in Modes : Begin(m)
    \/ RegistryDone \/ RegistryFails
    \/ \E o \in {"R", "Y"} : RegistryRedirect(o)
    \/ \E k \in {"basic", "bearerR", "bearerA", "bearerU"}, r \in {"token", "redirect"} :
          RegistryChallenge(k, r)
    \/ \E ok \in BOOLEAN : Fetch(ok)
    \/ Parse \/ Load \/ Finish
    \/ LocalEdit \/ UpstreamMoves \/ LockEdited \/ CacheChanged
    \/ Done

Spec == Init /\ [][Next]_vars

---------------------------------------------------------------------------------------------
\* Safety properties.

\* The manifest a compile uses is the bundle.toml bytes its content hash covered.
ManifestIsHashed == ~manifestUnhashed

\* No accepted tree held a link or special file when it was hashed: nothing readable through the
\* bundle that its hash skipped.
NoUnhashedContent == ~unhashedAccepted

\* Every paths.bin directory put on PATH is a real directory inside the hashed bundle when the
\* compile composes.
BinInsideBundle == ~binOutside

\* An OCI compile accepts only the tree of the layer its manifest digest names.
OciTreeIsDigests == ~notDigest

\* An OCI compile never accepts a tree holding .git or .stack under any spelling.
CacheHoldsNoExcluded == ~excludedAccepted

\* No value not checked to be a digest (or commit id) names a cache path or a request.
OnlyDigestsNamePaths == ~badNamed

\* A locked compile accepts exactly the pins and content hash of the stack.lock it read.
LockedAcceptsTheLock == ~lockedDrift

\* Only --update moves a locked pin (a moved tag or branch changes nothing until then).
PinsMoveOnlyByUpdate == ~pinMoved

\* Under required, a locked operation on a machine [lock] platforms does not list fetches nothing
\* and never reaches mise.
UnlistedHostRefusedFirst == ~unlistedRan

\* Under required, mise is reached by a locked operation, and stack.lock written by a compile,
\* only with every pin checked.
RequiredMeansCovered == ~uncovered

\* The operator's credentials go only to the origin they are bound to, and through it to its own
\* or an approved token service.
CredentialsOnlyWhereBound == credsTo \subseteq (IF cred = "bound" THEN {"R", "A"} ELSE {})

\* Authorization goes only to the origin it was issued for, never across a redirect.
AuthOnlyToItsOrigin == ~authCross

\* NOT promised (LIMIT config): the bundle files a tool runs from are those the compile hashed.
\* A local directory edited after a compile is read as it is then.
ToolsUsedAreHashed == accepted.on /\ transport = "local" => localVer = accepted.hash
====
