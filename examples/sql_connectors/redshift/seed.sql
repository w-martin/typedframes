-- Unquoted identifiers fold to lower case in both Postgres and Redshift, so writing
-- MixedCase here (or in the SELECT text in example.py) makes no difference to the
-- real, stored column names -- they end up as "orderid"/"revenue" either way.
CREATE TABLE Sales (
    OrderId INTEGER PRIMARY KEY,
    Revenue NUMERIC(10, 2) NOT NULL
);

INSERT INTO Sales (OrderId, Revenue) VALUES
    (1, 150.00),
    (2, 89.50),
    (3, 220.10);

CREATE TABLE products (
    product_id INTEGER PRIMARY KEY,
    unit_price NUMERIC(10, 2) NOT NULL,
    stock INTEGER NOT NULL
);

INSERT INTO products (product_id, unit_price, stock) VALUES
    (1, 19.99, 42),
    (2, 34.50, 17);
