# Snowflake semantic controls

These self-contained synthetic queries cover hierarchy scope, multi-argument
aggregates, lateral arguments, timestamp aliases, and inferred output types.
The verdicts below are predictions for engine verification, not recorded engine
results. Run `EXPLAIN USING TEXT` first to distinguish compile and runtime failures.

## Predicted valid

### H1: Hierarchy pseudo-column and operators

```sql
SELECT id, LEVEL - 1 AS depth, CONNECT_BY_ROOT id AS root_id,
       SYS_CONNECT_BY_PATH(id::VARCHAR, ',') AS order_path
FROM (SELECT 1::NUMBER(38,0) AS id, NULL::NUMBER(38,0) AS parent_id) orders
START WITH parent_id IS NULL
CONNECT BY parent_id = PRIOR id;
```

### A1: Multi-argument COUNT

```sql
SELECT COUNT(id, quantity, amount), COUNT(DISTINCT id, quantity)
FROM (SELECT 1::NUMBER(38,0) AS id, 2::NUMBER(38,0) AS quantity,
             3.5::FLOAT AS amount) orders;
```

### F1: FLATTEN cannot see its own VALUE output in its input argument

```sql
SELECT customer.value
FROM (SELECT '{"customers":[1,2]}'::VARCHAR AS value) orders,
LATERAL FLATTEN(input => PARSE_JSON(value):customers) customer;
```

### T1: Timestamp variants

```sql
SELECT CAST(ordered_at AS TIMESTAMP_NTZ),
       CAST(ordered_at AS TIMESTAMP_LTZ(9)),
       CAST(ordered_at AS TIMESTAMP_TZ)
FROM (SELECT '2026-01-01 12:00:00'::TIMESTAMP_NTZ AS ordered_at) orders;
```

### U1: Comments must not change UNION BY NAME column identity

```sql
WITH orders AS (
  SELECT 1::NUMBER(38,0) AS id, TRUE AS active, 2.5::FLOAT AS amount
), shipments AS (SELECT * FROM orders)
SELECT id,
-- availability
active,
-- subtotal
amount FROM shipments
UNION ALL BY NAME
SELECT amount, id,
-- availability
active FROM shipments
UNION ALL BY NAME
SELECT amount, id,
-- availability
active FROM shipments;
```

### D1: Timestamp conversions inside CASE feed DATE_PART

```sql
WITH orders AS (
  SELECT TRUE AS active, 0::NUMBER(38,0) AS quantity,
         '2026-01-01 12:00:00'::VARCHAR AS status
), shipments AS (
  SELECT CASE WHEN active THEN TO_TIMESTAMP(quantity)
              ELSE TRY_TO_TIMESTAMP(status) END AS shipped_at FROM orders
)
SELECT DATE_PART(EPOCH_SECOND, shipped_at) FROM shipments;
```

### D2: Date construction inside CASE feeds COALESCE

```sql
WITH orders AS (
  SELECT '2026-01-01'::DATE AS ordered_on, '2026-01-02'::VARCHAR AS status,
         TRUE AS active, 1::NUMBER(38,0) AS quantity
)
SELECT COALESCE(TRY_CAST(status AS DATE), ordered_on,
         CASE WHEN active THEN DATE_FROM_PARTS(YEAR(ordered_on)-quantity,1,1) END),
       COALESCE(ordered_on,
         CASE WHEN active THEN DATE_FROM_PARTS(YEAR(ordered_on)-quantity,1,1) END)
FROM orders;
```

### D3: ISO weekday extraction and DATEDIFF both return numbers

```sql
WITH orders AS (SELECT '2026-01-01'::DATE AS ordered_on), shipments AS (
  SELECT 5 + DAYOFWEEKISO(ordered_on) AS delivery_days,
         DATEDIFF(day,ordered_on,CURRENT_DATE) AS elapsed_days FROM orders
)
SELECT delivery_days >= elapsed_days FROM shipments;
```

## Predicted compile errors

```sql
SELECT missing FROM (SELECT 1 AS id) orders;
SELECT LEVEL FROM (SELECT 1 AS id) orders;
SELECT status, COUNT(id,quantity)
FROM (SELECT 1 AS id, 2 AS quantity, 'placed' AS status) orders;
SELECT value
FROM (SELECT PARSE_JSON('[1]') AS value) orders,
LATERAL FLATTEN(input => orders.value) customer;
SELECT CAST('2026-01-01' AS UNKNOWN_ORDER_TYPE);
SELECT TRUE AS result UNION ALL SELECT 1::NUMBER(38,0) AS result;
```

Unknown cast targets intentionally retain W213 because validation has no
authoritative installed-type catalogue. The other negative controls remain errors.
