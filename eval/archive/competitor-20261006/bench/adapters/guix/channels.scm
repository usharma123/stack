;; Guix channel pin (bench/research/guix.md): development commit whose package definitions
;; include Python 3.13. Inherits the default channel's authentication introduction; the
;; benchmark never passes --disable-authentication.
(use-modules (guix channels))
(list (channel
        (inherit %default-guix-channel)
        (url "https://git.guix.gnu.org/guix.git")
        (commit "71d010188f039817c465985e46e185445fda6946")))
