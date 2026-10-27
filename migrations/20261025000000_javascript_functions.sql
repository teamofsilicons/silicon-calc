CREATE TABLE javascript_functions (
    account_id uuid NOT NULL,
    name text NOT NULL,
    definition jsonb NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (account_id, name)
);
