CREATE TABLE receipts (
    id serial PRIMARY KEY,
    invoice_id integer NOT NULL REFERENCES invoices (id),
    cents integer NOT NULL,
    issued_at timestamptz NOT NULL DEFAULT now()
);
