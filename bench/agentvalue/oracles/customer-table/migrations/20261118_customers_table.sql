CREATE TABLE customers (
    id serial PRIMARY KEY,
    name text NOT NULL UNIQUE
);
INSERT INTO customers (name) SELECT DISTINCT customer FROM invoices ORDER BY customer;
ALTER TABLE invoices ADD COLUMN customer_id integer REFERENCES customers (id);
UPDATE invoices i SET customer_id = c.id FROM customers c WHERE c.name = i.customer;
ALTER TABLE invoices ALTER COLUMN customer_id SET NOT NULL;
ALTER TABLE invoices DROP COLUMN customer;
