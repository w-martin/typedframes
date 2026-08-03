SELECT order_id, amount
FROM 'orders.parquet'
WHERE amount > 100
