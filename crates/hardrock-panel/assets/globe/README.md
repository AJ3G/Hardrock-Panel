# GLOBE's map

From Natural Earth's 1:50m physical and cultural vectors
(https://www.naturalearthdata.com), which are in the public domain:

- `land.json`: land rings (holes for the larger lakes), simplified to about
  0.04°, as flat `[lon, lat, lon, lat, …]` lists.
- `borders.json`: country borders.
- `states.json`: state and province lines.
- `urban.json`: built-up areas as flat `[lon, lat, radius_km, …]` triples,
  for the city lights.
- `places.json`: populated places as `[name, lon, lat, tier]`; tier 3 over
  3 million people, 2 over a million, 1 over 300 000, 0 the rest.
