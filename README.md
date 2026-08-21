domino.garden
=============

pictures of domino for the public at https://domino.garden

## wow!

he's wonderful, i know

## how?
- static html site generated from rust with tera,
- blake3 to content-hash images,
- imagemagick to generate web-safe photos,
- rclone to sync images to backblaze-b2, 
- cloudflare pages for hosting/caching

## updating the site

- add images to `originals`
- `cargo run -- build && cargo run -- deploy`

## development

``` sh
# tailwind
tailwindcss -i styles/domino.css \
  -o public/assets/domino.css \
  --content 'templates/**/*.html' \
  --content 'public/**/*.html' \
  --watch
  
# webserver
caddy file-server --root public --listen :8080

# rust (generate images)
watchexec -e rs -- cargo run -- build
```

## caching

### media (domino photos)

we content-hash so we can set long `Cache-Control` headers:
```
Cache-Control: public, max-age=31536000, immutable
Content-Type: image/webp
X-Content-Type-Options: nosniff
```

### html (domino delivery)

this changes every time we add images so:
```
Cache-Control: public, max-age=300
```
