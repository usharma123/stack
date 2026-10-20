ALTER TABLE items ADD COLUMN tags text[] NOT NULL DEFAULT '{}';
CREATE INDEX items_checkout_idx ON items (checkout);
