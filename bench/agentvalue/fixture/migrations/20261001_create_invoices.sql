CREATE TABLE invoices (
    id serial PRIMARY KEY,
    customer text NOT NULL,
    cents integer NOT NULL CHECK (cents >= 0),
    status text NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'paid'))
);
