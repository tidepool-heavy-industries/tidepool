case (m2JoinA, m2JoinB, m2Shadow, m2MakeReply m2Shadow) of
  (M2JoinA a, M2JoinB b, M2Input current, M2Reply reply) ->
    (a, b, current, reply) == (43, 99, 99, 199)
