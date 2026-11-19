CREATE UNIQUE INDEX IF NOT EXISTS receipts_one_per_invoice ON receipts (invoice_id);
