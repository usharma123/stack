CREATE TABLE checkouts (
    name text PRIMARY KEY,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE items (
    id bigserial PRIMARY KEY,
    checkout text NOT NULL REFERENCES checkouts(name),
    sku text NOT NULL UNIQUE,
    title text NOT NULL,
    price_cents integer NOT NULL CHECK (price_cents >= 0),
    updated_at timestamptz NOT NULL DEFAULT now()
);
